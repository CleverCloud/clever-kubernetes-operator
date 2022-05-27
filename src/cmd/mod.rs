//! # Command module
//!
//! This module provide command line interface structures and helpers
use std::{io, net::AddrParseError, path::PathBuf, process::abort, sync::Arc};

use async_trait::async_trait;
use clap::{Parser, Subcommand};
use clevercloud_sdk::{
    oauth10a::{
        connector::HttpsConnector,
        proxy::{ProxyBuilder, ProxyConnectorBuilder},
        Credentials,
    },
    Client,
};
use hyper::{
    service::{make_service_fn, service_fn},
    Server,
};
use paw::ParseArgs;
use tracing::{error, info};

use crate::{
    cmd::crd::CustomResourceDefinitionError,
    svc::{
        cfg::Configuration,
        crd::{config_provider, elasticsearch, mongodb, mysql, postgresql, pulsar, redis},
        k8s::{client, State, Watcher},
        telemetry::router,
    },
};

pub mod crd;

// -----------------------------------------------------------------------------
// Executor trait

#[async_trait]
pub trait Executor {
    type Error;

    async fn execute(&self, config: Arc<Configuration>) -> Result<(), Self::Error>;
}

// -----------------------------------------------------------------------------
// CommandError enum

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to execute command '{0}', {1}")]
    Execution(String, Arc<Error>),
    #[error("failed to execute command, {0}")]
    CustomResourceDefinition(CustomResourceDefinitionError),
    #[error("failed to parse listen address '{0}', {1}")]
    Listen(String, AddrParseError),
    #[error("failed to handle termintion signal, {0}")]
    SigTerm(io::Error),
    #[error("failed to create kubernetes client, {0}")]
    Client(client::Error),
    #[error("failed to create clever cloud client, {0}")]
    CleverClient(clevercloud_sdk::oauth10a::proxy::Error),
}

// -----------------------------------------------------------------------------
// Command enum

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    /// Interact with custom resource definition
    #[clap(name = "custom-resource-definition", aliases= &["crd"], subcommand)]
    CustomResourceDefinition(crd::CustomResourceDefinition),
}

#[async_trait]
impl Executor for Command {
    type Error = Error;

    #[cfg_attr(feature = "trace", tracing::instrument)]
    async fn execute(&self, config: Arc<Configuration>) -> Result<(), Self::Error> {
        match self {
            Self::CustomResourceDefinition(crd) => crd
                .execute(config)
                .await
                .map_err(Error::CustomResourceDefinition)
                .map_err(|err| {
                    Error::Execution("custom-resource-definition".into(), Arc::new(err))
                }),
        }
    }
}

// -----------------------------------------------------------------------------
// Args struct

#[derive(Parser, Clone, Debug)]
#[clap(author, version, about)]
pub struct Args {
    /// Increase log verbosity
    #[clap(short = 'v', global = true, parse(from_occurrences))]
    pub verbosity: usize,
    /// Specify location of kubeconfig
    #[clap(short = 'k', long = "kubeconfig", global = true)]
    pub kubeconfig: Option<PathBuf>,
    /// Specify location of configuration
    #[clap(short = 'c', long = "config", global = true)]
    pub config: Option<PathBuf>,
    /// Check if configuration is healthy
    #[clap(short = 't', long = "check", global = true)]
    pub check: bool,
    #[clap(subcommand)]
    pub command: Option<Command>,
}

impl ParseArgs for Args {
    type Error = Error;

    fn parse_args() -> Result<Self, Self::Error> {
        Ok(Self::parse())
    }
}

// -----------------------------------------------------------------------------
// daemon function

#[cfg_attr(feature = "trace", tracing::instrument)]
pub async fn daemon(kubeconfig: Option<PathBuf>, config: Arc<Configuration>) -> Result<(), Error> {
    // -------------------------------------------------------------------------
    // Create a new kubernetes client from path if defined, or via the
    // environment or defaults locations
    let kube_client = client::try_new(kubeconfig).await.map_err(Error::Client)?;

    // -------------------------------------------------------------------------
    // Create a new clever-cloud client
    let credentials: Credentials = config.api.to_owned().into();
    let connector = match &config.proxy {
        Some(proxy) if proxy.https.is_some() || proxy.http.is_some() => {
            let proxy = ProxyBuilder::try_from(
                proxy.https.to_owned().unwrap_or_else(|| {
                    proxy
                        .http
                        .to_owned()
                        .expect("to have one of http or https value in proxy configuration file")
                }),
                proxy.no.to_owned(),
            )
            .map_err(Error::CleverClient)?;

            ProxyConnectorBuilder::default()
                .with_proxy(proxy)
                .build(HttpsConnector::new())
                .map_err(Error::CleverClient)?
        }
        _ => ProxyConnectorBuilder::try_from_env().map_err(Error::CleverClient)?,
    };

    let clever_client = Client::builder()
        .with_credentials(credentials)
        .build(connector);

    // -------------------------------------------------------------------------
    // Create state to give to each reconciler
    let postgresql_state = State::new(kube_client, clever_client, config.to_owned());
    let redis_state = postgresql_state.to_owned();
    let mysql_state = postgresql_state.to_owned();
    let mongodb_state = postgresql_state.to_owned();
    let pulsar_state = postgresql_state.to_owned();
    let config_provider_state = postgresql_state.to_owned();
    let elasticsearch_state = postgresql_state.to_owned();

    // -------------------------------------------------------------------------
    // Create reconcilers
    let handles = vec![
        tokio::spawn(async {
            let reconciler = postgresql::Reconciler::default();

            info!("Start to listen for events of postgresql addon custom resource");
            if let Err(err) = reconciler.watch(postgresql_state).await {
                error!(
                    "Could not reconcile postgresql addon custom resource, {}",
                    err
                );
            }

            abort();
        }),
        tokio::spawn(async {
            let reconciler = redis::Reconciler::default();

            info!("Start to listen for events of redis addon custom resource");
            if let Err(err) = reconciler.watch(redis_state).await {
                error!("Could not reconcile redis addon custom resource, {}", err);
            }

            abort();
        }),
        tokio::spawn(async {
            let reconciler = mysql::Reconciler::default();

            info!("Start to listen for events of mysql addon custom resource");
            if let Err(err) = reconciler.watch(mysql_state).await {
                error!("Could not reconcile mysql addon custom resource, {}", err);
            }

            abort();
        }),
        tokio::spawn(async {
            let reconciler = mongodb::Reconciler::default();

            info!("Start to listen for events of mongodb addon custom resource");
            if let Err(err) = reconciler.watch(mongodb_state).await {
                error!("Could not reconcile mongodb addon custom resource, {}", err);
            }

            abort();
        }),
        tokio::spawn(async {
            let reconciler = pulsar::Reconciler::default();

            info!("Start to listen for events of pulsar addon custom resource");
            if let Err(err) = reconciler.watch(pulsar_state).await {
                error!("Could not reconcile plusar addon custom resource, {}", err);
            }

            abort();
        }),
        tokio::spawn(async {
            let reconciler = config_provider::Reconciler::default();

            info!("Start to listen for events of config-provider addon custom resource");
            if let Err(err) = reconciler.watch(config_provider_state).await {
                error!(
                    "Could not reconcile config-provider addon custom resource, {}",
                    err
                );
            }

            abort();
        }),
        tokio::spawn(async {
            let reconciler = elasticsearch::Reconciler::default();

            info!("Start to listen for events of elasticsearch addon custom resource");
            if let Err(err) = reconciler.watch(elasticsearch_state).await {
                error!(
                    "Could not reconcile elasticsearch addon custom resource, {}",
                    err
                );
            }

            abort();
        }),
    ];

    // -------------------------------------------------------------------------
    // Create http server
    let addr = config
        .operator
        .listen
        .parse()
        .map_err(|err| Error::Listen(config.operator.listen.to_owned(), err))?;

    let server = tokio::spawn(async move {
        let builder = match Server::try_bind(&addr) {
            Ok(builder) => builder,
            Err(err) => {
                error!("Could not bind http server, {}", err);
                abort();
            }
        };

        let server = builder.serve(make_service_fn(|_| async {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(service_fn(router))
        }));

        info!("Start to listen for http request on {}", addr);
        if let Err(err) = server.await {
            error!("Could not serve http server, {}", err);
        }

        abort()
    });

    // -------------------------------------------------------------------------
    // Wait for termination signal
    tokio::signal::ctrl_c().await.map_err(Error::SigTerm)?;

    // -------------------------------------------------------------------------
    // Cancel reconcilers
    handles.iter().for_each(|handle| handle.abort());

    for handle in handles {
        if let Err(err) = handle.await {
            if !err.is_cancelled() {
                error!("Could not wait for the task to complete, {}", err);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Cancel http server
    server.abort();
    if let Err(err) = server.await {
        if !err.is_cancelled() {
            error!(
                "Could not wait for the http server to gracefully close, {}",
                err
            );
        }
    }

    Ok(())
}
