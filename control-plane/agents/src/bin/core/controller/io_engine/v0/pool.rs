use super::translation::{rpc_pool_to_agent, AgentToIoEngine};
use agents::errors::{GrpcRequest as GrpcRequestError, SvcError};
use rpc::io_engine::Null;
use stor_port::{
    transport_api::ResourceKind,
    types::v0::transport::{CreatePool, DestroyPool, ImportPool, PoolState},
};

use snafu::ResultExt;

#[async_trait::async_trait]
impl crate::controller::io_engine::PoolListApi for super::RpcClient {
    async fn list_pools(&self) -> Result<Vec<PoolState>, SvcError> {
        let rpc_pools = self
            .client()
            .list_pools(Null {})
            .await
            .context(GrpcRequestError {
                resource: ResourceKind::Pool,
                request: "list_pools",
            })?;

        let rpc_pools = &rpc_pools.get_ref().pools;

        let pools = rpc_pools
            .iter()
            .map(|p| rpc_pool_to_agent(p, self.context.node()))
            .collect();

        Ok(pools)
    }
}

#[async_trait::async_trait]
impl crate::controller::io_engine::PoolApi for super::RpcClient {
    async fn create_pool(&self, request: &CreatePool) -> Result<PoolState, SvcError> {
        match self.client().create_pool(request.to_rpc()).await {
            Ok(rpc_pool) => {
                let pool = rpc_pool_to_agent(&rpc_pool.into_inner(), &request.node);
                Ok(pool)
            }
            Err(error)
                if error.code() == tonic::Code::Internal
                    && error.message()
                        == format!(
                            "Failed to create a BDEV '{}'",
                            request.disks.first().cloned().unwrap_or_default()
                        ) =>
            {
                Err(SvcError::GrpcRequestError {
                    resource: ResourceKind::Pool,
                    request: "create_pool".to_string(),
                    source: tonic::Status::not_found(error.message()),
                })
            }
            Err(error) => Err(error).context(GrpcRequestError {
                resource: ResourceKind::Pool,
                request: "create_pool",
            }),
        }
    }

    async fn destroy_pool(&self, request: &DestroyPool) -> Result<(), SvcError> {
        let _ = self
            .client()
            .destroy_pool(request.to_rpc())
            .await
            .context(GrpcRequestError {
                resource: ResourceKind::Pool,
                request: "destroy_pool",
            })?;
        Ok(())
    }

    async fn import_pool(&self, _request: &ImportPool) -> Result<PoolState, SvcError> {
        Err(SvcError::GrpcRequestError {
            resource: ResourceKind::Pool,
            request: "import_pool".to_string(),
            source: tonic::Status::unimplemented(""),
        })
    }
}
