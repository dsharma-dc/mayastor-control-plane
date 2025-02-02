//! IoEngine CSI plugin.
//!
//! Implementation of gRPC methods from the CSI spec. This includes mounting
//! of volumes using iscsi/nvmf protocols on the node.

use crate::{
    error::FsfreezeError,
    fsfreeze::{bin::fsfreeze, FsFreezeOpt},
    identity::Identity,
    k8s::patch_k8s_node,
    mount::probe_filesystems,
    node::Node,
    nodeplugin_grpc::NodePluginGrpcServer,
    nodeplugin_nvme::NvmeOperationsSvc,
    registration::run_registration_loop,
    shutdown_event::Shutdown,
};
use csi_driver::csi::{identity_server::IdentityServer, node_server::NodeServer};
use grpc::csi_node_nvme::nvme_operations_server::NvmeOperationsServer;
use stor_port::platform;
use utils::tracing_telemetry::{FmtLayer, FmtStyle};

use crate::client::AppNodesClientWrapper;
use clap::Arg;
use futures::TryFutureExt;
use serde_json::json;
use std::{
    collections::HashMap,
    env, fs,
    future::Future,
    io::ErrorKind,
    net::SocketAddr,
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::UnixListener,
};
use tonic::transport::{server::Connected, Server};
use tracing::{debug, error, info};

#[derive(Clone, Debug)]
pub struct UdsConnectInfo {
    pub peer_addr: Option<Arc<tokio::net::unix::SocketAddr>>,
    pub peer_cred: Option<tokio::net::unix::UCred>,
}

#[derive(Debug)]
struct UnixStream(tokio::net::UnixStream);

impl Connected for UnixStream {
    type ConnectInfo = UdsConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        UdsConnectInfo {
            peer_addr: self.0.peer_addr().ok().map(Arc::new),
            peer_cred: self.0.peer_cred().ok(),
        }
    }
}

impl AsyncRead for UnixStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for UnixStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

const GRPC_PORT: u16 = 50051;

pub(super) async fn main() -> anyhow::Result<()> {
    // Parse command line arguments.
    let matches = clap::Command::new(utils::package_description!())
        .about("k8s sidecar for IoEngine implementing CSI among others")
        .version(utils::version_info_str!())
        .subcommand_negates_reqs(true)
        .arg(
            Arg::new("rest-endpoint")
                .long("rest-endpoint")
                .env("ENDPOINT")
                .help("A URL endpoint to the control plane's rest endpoint")
        )
        .arg(
            Arg::new("enable-registration")
                .long("enable-registration")
                .action(clap::ArgAction::SetTrue)
                .value_name("BOOLEAN")
                .requires("rest-endpoint")
                .help("Enable registration of the csi node with the control plane")
        )
        .arg(
            Arg::new("csi-socket")
                .short('c')
                .long("csi-socket")
                .value_name("PATH")
                .help("CSI gRPC listen socket (default /var/tmp/csi.sock)")
        )
        .arg(
            Arg::new("node-name")
                .short('n')
                .long("node-name")
                .value_name("NAME")
                .help("Unique node name where this instance runs")
                .required(true)
        )
        .arg(
            Arg::new("grpc-endpoint")
                .short('g')
                .long("grpc-endpoint")
                .value_name("ENDPOINT")
                .help("ip address where this instance runs, and optionally the gRPC port")
                .default_value("0.0.0.0")
                .required(false)
        )
        .arg(
            Arg::new("v")
                .short('v')
                .action(clap::ArgAction::Count)
                .help("Sets the verbosity level")
        )
        .arg(
            Arg::new("nvme-core-io-timeout")
                .long("nvme-core-io-timeout")
                .value_name("TIMEOUT")
                .required(false)
                .help("Sets the global nvme_core module io_timeout, in seconds or humantime")
        )
        .arg(
            Arg::new("nvme-io-timeout")
                .long("nvme-io-timeout")
                .required(false)
                .help("Sets io_timeout for nvme block devices")
        )
        .arg(
            Arg::new(crate::config::nvme_nr_io_queues())
                .long(crate::config::nvme_nr_io_queues())
                .value_name("NUMBER")
                .required(false)
                .help("Sets the nvme-nr-io-queues parameter when connecting to a volume target")
        )
        .arg(
            Arg::new(crate::config::nvme_ctrl_loss_tmo())
                .long(crate::config::nvme_ctrl_loss_tmo())
                .value_name("NUMBER")
                .required(false)
                .help("Sets the nvme-ctrl-loss-tmo parameter when connecting to a volume target. (May be overridden through the storage class)")
        )
        .arg(
            Arg::new(crate::config::nvme_keep_alive_tmo())
                .long(crate::config::nvme_keep_alive_tmo())
                .value_name("NUMBER")
                .required(false)
                .help("Sets the nvme-keep-alive-tmo parameter when connecting to a volume target")
        )
        .arg(
            Arg::new("node-selector")
                .long("node-selector")
                .action(clap::ArgAction::Append)
                .num_args(1)
                .allow_hyphen_values(true)
                .default_value(csi_driver::csi_node_selector())
                .help(
                    "The node selector label which this plugin will report as part of its topology.\n\
                    Example:\n --node-selector key=value --node-selector key2=value2",
                ),
        )
        .subcommand(
            clap::Command::new("fs-freeze")
                .arg(
                    Arg::new("volume-id")
                        .short('v')
                        .long("volume-id")
                        .value_name("UUID")
                        .required(true)
                        .help("Uuid of the volume to freeze")
                )
        )
        .subcommand(
            clap::Command::new("fs-unfreeze")
                .arg(
                    Arg::new("volume-id")
                        .short('v')
                        .long("volume-id")
                        .value_name("UUID")
                        .required(true)
                        .help("Uuid of the volume to unfreeze")
                )
        )
        .get_matches();

    // Handle fs-freeze and fs-unfreeze commands.
    if let Some(cmd) = matches.subcommand() {
        utils::tracing_telemetry::TracingTelemetry::builder()
            .with_writer(FmtLayer::Stderr)
            .with_style(FmtStyle::Compact)
            .with_colours(false)
            .init();
        match cmd {
            ("fs-freeze", arg_matches) => {
                let volume_id = arg_matches.get_one::<String>("volume-id").unwrap();
                fsfreeze(volume_id, FsFreezeOpt::Freeze).await
            }
            ("fs-unfreeze", arg_matches) => {
                let volume_id = arg_matches.get_one::<String>("volume-id").unwrap();
                fsfreeze(volume_id, FsFreezeOpt::Unfreeze).await
            }
            _ => Err(FsfreezeError::InvalidFreezeCommand),
        }?;
        return Ok(());
    }

    // Print package info and command line arguments.
    utils::print_package_info!();
    println!("{:?}", env::args().collect::<Vec<String>>());

    // Initialize tracing telemetry.
    let tags = utils::tracing_telemetry::default_tracing_tags(
        utils::raw_version_str(),
        env!("CARGO_PKG_VERSION"),
    );
    utils::tracing_telemetry::init_tracing("csi-node", tags, None);

    // Validate presence of nvme_tcp kernel module and set nvme_core parameters.
    if let Err(error) = crate::dev::nvmf::check_nvme_tcp_module() {
        anyhow::bail!("Failed to detect nvme_tcp kernel module. Run `modprobe nvme_tcp` to load the kernel module. {}", error);
    }
    if let Some(nvme_io_timeout) = matches.get_one::<String>("nvme-io-timeout") {
        let _ = humantime::Duration::from_str(nvme_io_timeout)
            .map_err(|error| anyhow::format_err!("Failed to parse 'nvme-io-timeout': {error}"))?;
    };
    if let Some(nvme_io_timeout) = matches.get_one::<String>("nvme-core-io-timeout") {
        let io_timeout_secs = match humantime::Duration::from_str(nvme_io_timeout) {
            Ok(human_time) => {
                human_time.as_secs() as u32
            }
            Err(_) => {
                nvme_io_timeout.parse().expect(
                    "nvme_core io_timeout should be in humantime or an integer number, representing the timeout in seconds",
                )
            }
        };

        if let Err(error) = crate::dev::nvmf::set_nvmecore_iotimeout(io_timeout_secs) {
            anyhow::bail!("Failed to set nvme_core io_timeout: {}", error);
        }
    }

    // Check for nvme ana multipath support and generate labels.
    let mut csi_labels = HashMap::new();
    let nvme_enabled = utils::check_nvme_core_ana().unwrap_or_default().to_string();
    csi_labels.insert(utils::CSI_NODE_NVME_ANA.to_string(), nvme_enabled.clone());

    // If running in k8s, label the nodes with generated labels.
    let node_name = matches.get_one::<String>("node-name").unwrap();
    if platform::current_platform_type() == platform::PlatformType::K8s {
        let kube_client = kube::Client::try_default().await?;
        check_ana_and_label_node(&kube_client, node_name, nvme_enabled).await?;
    }

    // Parse the CSI socket file name from the command line arguments.
    let csi_socket = matches
        .get_one::<String>("csi-socket")
        .map(|s| s.as_str())
        .unwrap_or("/var/tmp/csi.sock");
    // Remove stale CSI socket from previous instance if there is any.
    match fs::remove_file(csi_socket) {
        Ok(_) => info!("Removed stale CSI socket {}", csi_socket),
        Err(err) => {
            if err.kind() != ErrorKind::NotFound {
                anyhow::bail!("Error removing stale CSI socket {}: {}", csi_socket, err);
            }
        }
    }

    // Initialize the rest api client.
    let client = AppNodesClientWrapper::initialize(matches.get_one::<String>("rest-endpoint"))?;

    let registration_enabled = matches.get_flag("enable-registration");

    // Parse instance and grpc endpoints from the command line arguments and validate.
    let grpc_sock_addr = validate_endpoints(
        matches.get_one::<String>("grpc-endpoint").unwrap(),
        registration_enabled,
    )?;

    // Start the CSI server, node plugin grpc server and registration loop if registration is
    // enabled.
    *crate::config::config().nvme_as_mut() = TryFrom::try_from(&matches)?;
    let (csi, grpc, registration) = tokio::join!(
        CsiServer::run(csi_socket, &matches)?,
        NodePluginGrpcServer::run(grpc_sock_addr),
        run_registration_loop(
            node_name.clone(),
            grpc_sock_addr.to_string(),
            Some(csi_labels),
            &client,
            registration_enabled
        )
    );
    vec![csi, grpc, registration].into_iter().collect()
}

struct CsiServer {}

impl CsiServer {
    fn run(
        csi_socket: &str,
        cli_args: &clap::ArgMatches,
    ) -> anyhow::Result<impl Future<Output = anyhow::Result<()>>> {
        let node_name = cli_args.get_one::<String>("node-name").expect("required");
        let node_selector = csi_driver::csi_node_selector_parse(
            cli_args
                .get_many::<String>("node-selector")
                .map(|s| s.map(|s| s.as_str())),
        )?;

        let incoming = {
            let uds = UnixListener::bind(csi_socket).unwrap();
            info!("CSI plugin bound to {}", csi_socket);

            // Change permissions on CSI socket to allow non-privileged clients to access it
            // to simplify testing.
            if let Err(e) = fs::set_permissions(
                csi_socket,
                std::os::unix::fs::PermissionsExt::from_mode(0o777),
            ) {
                error!("Failed to change permissions for CSI socket: {:?}", e);
            } else {
                debug!("Successfully changed file permissions for CSI socket");
            }

            async_stream::stream! {
                loop {
                    let item = uds.accept().map_ok(|(st, _)| UnixStream(st)).await;
                    yield item;
                }
            }
        };

        let node = Node::new(node_name.into(), node_selector, probe_filesystems());
        Ok(async move {
            Server::builder()
                .add_service(NodeServer::new(node))
                .add_service(IdentityServer::new(Identity {}))
                .add_service(NvmeOperationsServer::new(NvmeOperationsSvc {}))
                .serve_with_incoming_shutdown(incoming, Shutdown::wait())
                .await
                .map_err(|error| {
                    use stor_port::transport_api::ErrorChain;
                    error!(error = error.full_string(), "CsiServer failed");
                    error.into()
                })
        })
    }
}

/// Gets the nvme ana multipath parameter and sets on the node as a label.
async fn check_ana_and_label_node(
    client: &kube::client::Client,
    node_name: &str,
    nvme_enabled: String,
) -> anyhow::Result<()> {
    let node_patch = json!({
        "apiVersion": "v1",
        "kind": "Node",
        "metadata": {
            "labels": {
                utils::CSI_NODE_NVME_ANA: nvme_enabled,
            },
        },
    });
    patch_k8s_node(client, node_name, &node_patch).await?;
    Ok(())
}

/// Validate that the grpc endpoint is valid.
fn validate_endpoints(
    grpc_endpoint: &str,
    registration_enabled: bool,
) -> anyhow::Result<SocketAddr> {
    let grpc_endpoint = if grpc_endpoint.contains(':') {
        grpc_endpoint.to_string()
    } else {
        format!("{grpc_endpoint}:{GRPC_PORT}")
    };
    let grpc_endpoint_url = SocketAddr::from_str(&grpc_endpoint)?;
    // Should not allow using an unspecified ip if registration is enabled as grpc endpoint gets
    // sent in registration request.
    if registration_enabled && grpc_endpoint_url.ip().is_unspecified() {
        return Err(anyhow::format_err!(
            "gRPC endpoint: `0.0.0.0` is not allowed if registration is enabled"
        ));
    }
    Ok(grpc_endpoint_url)
}
