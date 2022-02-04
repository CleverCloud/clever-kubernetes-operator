//! # Pulsar addon
//!
//! This module provide the puslar custom resource and its definition

use std::{
    fmt::{self, Display, Formatter},
    sync::Arc,
};

use async_trait::async_trait;
use clevercloud_sdk::{
    v2::{
        self,
        addon::{AddonOpts, CreateAddonOpts},
    },
    v4::{self, addon_provider::AddonProviderId},
};
use futures::TryFutureExt;
use kube::{api::ListParams, Api, Resource, ResourceExt};
use kube_derive::CustomResource;
use kube_runtime::{
    controller::{self, Context},
    watcher, Controller,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use slog_scope::{debug, error, info};

use crate::svc::{
    clevercloud::{self, ext::AddonExt},
    k8s::{self, finalizer, recorder, resource, secret, ControllerBuilder, State},
};

// -----------------------------------------------------------------------------
// Constants

pub const ADDON_FINALIZER: &str = "api.clever-cloud.com/pulsar";
pub const ADDON_BETA_PLAN: &str = "plan_3ad3c5be-5c1e-4dae-bf9a-87120b88fc13";

// -----------------------------------------------------------------------------
// PulsarInstance structure

#[derive(JsonSchema, Serialize, Deserialize, PartialEq, Clone, Debug)]
pub struct PulsarInstance {
    #[serde(rename = "region")]
    pub region: String,
}

// -----------------------------------------------------------------------------
// PulsarSpec structure

#[derive(CustomResource, JsonSchema, Serialize, Deserialize, PartialEq, Clone, Debug)]
#[kube(group = "api.clever-cloud.com")]
#[kube(version = "v1beta1")]
#[kube(kind = "Pulsar")]
#[kube(singular = "pulsar")]
#[kube(plural = "pulsars")]
#[kube(shortname = "pulse")]
#[kube(shortname = "pul")]
#[kube(status = "PulsarStatus")]
#[kube(namespaced)]
#[kube(apiextensions = "v1")]
#[kube(derive = "PartialEq")]
pub struct PulsarSpec {
    #[serde(rename = "organisation")]
    pub organisation: String,
    #[serde(rename = "instance")]
    pub instance: PulsarInstance,
}

// -----------------------------------------------------------------------------
// PulsarStatus structure

#[derive(JsonSchema, Serialize, Deserialize, PartialEq, Clone, Debug, Default)]
pub struct PulsarStatus {
    #[serde(rename = "addon")]
    pub addon: Option<String>,
}

// -----------------------------------------------------------------------------
// Pulsar implementation

#[allow(clippy::from_over_into)]
impl Into<CreateAddonOpts> for Pulsar {
    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn into(self) -> CreateAddonOpts {
        CreateAddonOpts {
            name: AddonExt::name(&self),
            region: self.spec.instance.region.to_owned(),
            provider_id: AddonProviderId::Pulsar.to_string(),
            plan: ADDON_BETA_PLAN.to_string(),
            options: AddonOpts::default(),
        }
    }
}

impl AddonExt for Pulsar {
    type Error = ReconcilerError;

    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn id(&self) -> Option<String> {
        if let Some(status) = &self.status {
            return status.addon.to_owned();
        }

        None
    }

    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn organisation(&self) -> String {
        self.spec.organisation.to_owned()
    }

    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn name(&self) -> String {
        "kubernetes_".to_string()
            + &self
                .uid()
                .expect("expect all resources in kubernetes to have an identifier")
    }
}

impl Pulsar {
    #[cfg_attr(feature = "trace", tracing::instrument)]
    pub fn set_addon_id(&mut self, id: Option<String>) {
        let mut status = self.status.get_or_insert_with(PulsarStatus::default);

        status.addon = id;
        self.status = Some(status.to_owned());
    }

    #[cfg_attr(feature = "trace", tracing::instrument)]
    pub fn get_addon_id(&self) -> Option<String> {
        self.status.to_owned().unwrap_or_default().addon
    }
}

// -----------------------------------------------------------------------------
// PulsarAction structure

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Debug)]
pub enum PulsarAction {
    UpsertFinalizer,
    UpsertAddon,
    UpsertSecret,
    DeleteFinalizer,
    DeleteAddon,
}

impl Display for PulsarAction {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::UpsertFinalizer => write!(f, "UpsertFinalizer"),
            Self::UpsertAddon => write!(f, "UpsertAddon"),
            Self::UpsertSecret => write!(f, "UpsertSecret"),
            Self::DeleteFinalizer => write!(f, "DeleteFinalizer"),
            Self::DeleteAddon => write!(f, "DeleteAddon"),
        }
    }
}

// -----------------------------------------------------------------------------
// ReconcilerError enum

#[derive(thiserror::Error, Debug)]
pub enum ReconcilerError {
    #[error("failed to reconcile resource, {0}")]
    Reconcile(String),
    #[error("failed to execute request on clever-cloud api, {0}")]
    CleverClient(clevercloud::Error),
    #[error("failed to execute request on kubernetes api, {0}")]
    KubeClient(kube::Error),
    #[error("failed to compute diff between the original and modified object, {0}")]
    Diff(serde_json::Error),
}

impl From<kube::Error> for ReconcilerError {
    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn from(err: kube::Error) -> Self {
        Self::KubeClient(err)
    }
}

impl From<clevercloud::Error> for ReconcilerError {
    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn from(err: clevercloud::Error) -> Self {
        Self::CleverClient(err)
    }
}

impl From<v2::addon::Error> for ReconcilerError {
    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn from(err: v2::addon::Error) -> Self {
        Self::from(clevercloud::Error::from(err))
    }
}

impl From<v4::addon_provider::plan::Error> for ReconcilerError {
    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn from(err: v4::addon_provider::plan::Error) -> Self {
        Self::from(clevercloud::Error::from(err))
    }
}

impl From<controller::Error<Self, watcher::Error>> for ReconcilerError {
    #[cfg_attr(feature = "trace", tracing::instrument)]
    fn from(err: controller::Error<ReconcilerError, watcher::Error>) -> Self {
        Self::Reconcile(err.to_string())
    }
}

// -----------------------------------------------------------------------------
// Reconciler structure

#[derive(Clone, Default, Debug)]
pub struct Reconciler {}

impl ControllerBuilder<Pulsar> for Reconciler {
    fn build(&self, state: State) -> Controller<Pulsar> {
        Controller::new(Api::all(state.kube), ListParams::default())
    }
}

#[async_trait]
impl k8s::Reconciler<Pulsar> for Reconciler {
    type Error = ReconcilerError;

    async fn upsert(ctx: &Context<State>, origin: Arc<Pulsar>) -> Result<(), ReconcilerError> {
        let State {
            kube,
            apis,
            config: _,
        } = ctx.get_ref();
        let kind = Pulsar::kind(&()).to_string();
        let (namespace, name) = resource::namespaced_name(&*origin);

        // ---------------------------------------------------------------------
        // Step 1: set finalizer

        info!("Set finalizer on custom resource"; "kind" => &kind, "uid" => &origin.meta().uid,"name" => &name, "namespace" => &namespace);
        let modified = finalizer::add((*origin).to_owned(), ADDON_FINALIZER);

        debug!("Update information of custom resource"; "kind" => &kind, "uid" => &modified.meta().uid,"name" => &name, "namespace" => &namespace);
        let patch = resource::diff(&*origin, &modified).map_err(ReconcilerError::Diff)?;
        let mut modified = resource::patch(kube.to_owned(), &modified, patch).await?;

        let action = &PulsarAction::UpsertFinalizer;
        let message = &format!("Create finalizer '{}'", ADDON_FINALIZER);
        recorder::normal(kube.to_owned(), &modified, action, message).await?;

        // ---------------------------------------------------------------------
        // Step 2:

        // This is not the step that you are looking for.

        // ---------------------------------------------------------------------
        // Step 3: upsert addon

        info!("Upsert addon for custom resource"; "kind" => &kind, "uid" => &modified.meta().uid,"name" => &name, "namespace" => &namespace);
        let addon = modified.upsert(apis).await?;

        modified.set_addon_id(Some(addon.id.to_owned()));

        debug!("Update information and status of custom resource"; "kind" => &kind, "uid" => &modified.meta().uid,"name" => &name, "namespace" => &namespace);
        let patch = resource::diff(&*origin, &modified).map_err(ReconcilerError::Diff)?;
        let modified = resource::patch(kube.to_owned(), &modified, patch.to_owned())
            .and_then(|modified| resource::patch_status(kube.to_owned(), modified, patch))
            .await?;

        let action = &PulsarAction::UpsertAddon;
        let message = &format!(
            "Create managed pulsar instance on clever-cloud '{}'",
            addon.id
        );
        recorder::normal(kube.to_owned(), &modified, action, message).await?;

        // ---------------------------------------------------------------------
        // Step 4: create the secret

        let secrets = modified.secrets(apis).await?;
        if let Some(secrets) = secrets {
            let s = secret::new(&modified, secrets);
            let (s_ns, s_name) = resource::namespaced_name(&s);

            info!("Upsert kubernetes secret resource for custom resource"; "kind" => &kind, "uid" => &modified.meta().uid,"name" => &name, "namespace" => &namespace);
            info!("Upsert kubernetes secret"; "kind" => "Secret", "name" => &s_name, "namespace" => &s_ns);
            let secret = resource::upsert(kube.to_owned(), &s, false).await?;

            let action = &PulsarAction::UpsertSecret;
            let message = &format!("Create kubernetes secret '{}'", secret.name());
            recorder::normal(kube.to_owned(), &modified, action, message).await?;
        }

        Ok(())
    }

    async fn delete(ctx: &Context<State>, origin: Arc<Pulsar>) -> Result<(), ReconcilerError> {
        let State {
            apis,
            kube,
            config: _,
        } = ctx.get_ref();
        let mut modified = (*origin).to_owned();
        let kind = Pulsar::kind(&()).to_string();
        let (namespace, name) = resource::namespaced_name(&*origin);

        // ---------------------------------------------------------------------
        // Step 1: delete the addon

        info!("Delete addon for custom resource"; "kind" => &kind, "uid" => &modified.meta().uid,"name" => &name, "namespace" => &namespace);
        modified.delete(apis).await?;
        modified.set_addon_id(None);

        debug!("Update information and status of custom resource"; "kind" => &kind, "uid" => &modified.meta().uid,"name" => &name, "namespace" => &namespace);
        let patch = resource::diff(&*origin, &modified).map_err(ReconcilerError::Diff)?;
        let modified = resource::patch(kube.to_owned(), &modified, patch.to_owned())
            .and_then(|modified| resource::patch_status(kube.to_owned(), modified, patch))
            .await?;

        let action = &PulsarAction::DeleteAddon;
        let message = "Delete managed pulsar instance on clever-cloud";
        recorder::normal(kube.to_owned(), &modified, action, message).await?;

        // ---------------------------------------------------------------------
        // Step 2: remove the finalizer

        info!("Remove finalizer on custom resource"; "kind" => &kind, "uid" => &modified.meta().uid,"name" => &name, "namespace" => &namespace);
        let modified = finalizer::remove(modified, ADDON_FINALIZER);

        let action = &PulsarAction::DeleteFinalizer;
        let message = "Delete finalizer from custom resource";
        recorder::normal(kube.to_owned(), &modified, action, message).await?;

        debug!("Update information of custom resource"; "kind" => &kind, "uid" => &modified.meta().uid,"name" => &name, "namespace" => &namespace);
        let patch = resource::diff(&*origin, &modified).map_err(ReconcilerError::Diff)?;
        resource::patch(kube.to_owned(), &modified, patch.to_owned()).await?;

        Ok(())
    }
}
