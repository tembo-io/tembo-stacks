mod postgresclusters;

use kube::api::{DeleteParams, ListParams, Patch, PatchParams};
use kube::{Api, Client, CustomResource};
use postgresclusters::PostgresCluster;
use serde::{Deserialize, Serialize};
use std::fmt;
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

pub struct CoreDBDeploymentService {}

impl CoreDBDeploymentService {
    pub async fn get_all(client: Client) -> Vec<PostgresCluster> {
        let pg_cluster_api: Api<PostgresCluster> = Api::default_namespaced(client);
        let mut pg_cluster_vec: Vec<PostgresCluster> = Vec::new();
        let pg_list = pg_cluster_api
            .list(&ListParams::default())
            .await
            .expect("could not get PostgresCluster");
        pg_list.items
    }

    // TODO(ianstanton) pull name from deployment JSON
    pub async fn create_or_update(
        client: Client,
        namespace: String,
        deployment: serde_json::Value,
    ) -> Result<(), Error> {
        let pg_cluster_api: Api<PostgresCluster> = Api::default_namespaced(client);
        let params = PatchParams::apply("deployment-service").force();
        let _o = pg_cluster_api
            .patch("dummy", &params, &Patch::Apply(&deployment))
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }

    // TODO(ianstanton) pull name from deployment JSON
    pub async fn delete(
        client: Client,
        namespace: String,
        deployment: serde_json::Value,
    ) -> Result<(), Error> {
        let pg_cluster_api: Api<PostgresCluster> = Api::default_namespaced(client);
        let params = DeleteParams::default();
        let _o = pg_cluster_api
            .delete("dummy", &params)
            .await
            .map_err(Error::KubeError);
        Ok(())
    }
}
