//! # Client module
//!
//! This module provide an helper to create a kubernetes client

use std::{convert::TryFrom, path::PathBuf};

use kube::{
    Config,
    config::{KubeConfigOptions, Kubeconfig, KubeconfigError},
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to read kubernetes configuration file, {0}")]
    Kubeconfig(KubeconfigError),
    #[error("failed to create kubernetes client, {0}")]
    CreateClient(kube::Error),
}

#[cfg_attr(feature = "tracing", tracing::instrument)]
/// returns a new kubernetes client from the given path if defined
/// or retrieve it from environment or defaults paths
pub async fn try_new(path: Option<PathBuf>) -> Result<kube::Client, Error> {
    match path {
        None => kube::Client::try_default()
            .await
            .map_err(Error::CreateClient),
        Some(path) => {
            let kubeconfig = Kubeconfig::read_from(path).map_err(Error::Kubeconfig)?;
            let opts = KubeConfigOptions::default();
            let config = Config::from_custom_kubeconfig(kubeconfig, &opts)
                .await
                .map_err(Error::Kubeconfig)?;

            kube::Client::try_from(config).map_err(Error::CreateClient)
        }
    }
}
