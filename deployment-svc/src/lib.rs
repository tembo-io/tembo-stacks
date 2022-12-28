use kube::api::{DeleteParams, ListParams, ObjectList, Patch, PatchParams};
use kube::{Api, CustomResource};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error),

    #[error("Kube Error: {0}")]
    KubeError(#[source] kube::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}

// TODO(ianstanton) This may not be necessary, but for now we need to define Api type PostgresCluster somewhere
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "postgres-operator.crunchydata.com",
    version = "v1beta1",
    kind = "PostgresCluster",
    namespaced
)]
pub struct PostgresClusterSpec {}

pub struct CoreDBDeploymentService {}

impl CoreDBDeploymentService {
    pub async fn get_all(pg_cluster_api: Api<PostgresCluster>) -> Vec<ObjectList<PostgresCluster>> {
        let mut pg_cluster_vec: Vec<ObjectList<PostgresCluster>> = Vec::new();

        while let Ok(pg) = pg_cluster_api.list(&ListParams::default()).await {
            pg_cluster_vec.push(pg);
        }
        pg_cluster_vec
    }

    pub async fn create(
        pg_cluster_api: Api<PostgresCluster>,
        deployment: serde_json::Value,
    ) -> Result<(), Error> {
        let params = PatchParams::apply("deployment-service").force();
        let _o = pg_cluster_api
            .patch("dummy", &params, &Patch::Apply(&deployment))
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }

    pub async fn delete(
        pg_cluster_api: Api<PostgresCluster>,
        deployment: serde_json::Value,
    ) -> Result<(), Error> {
        let params = DeleteParams::default();
        let _o = pg_cluster_api
            .delete("dummy", &params)
            .await
            .map_err(Error::KubeError);
        Ok(())
    }
}
