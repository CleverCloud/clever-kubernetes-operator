//! # Secret module
//!
//! This module provide helpers to generate secrets from a custom resource

use std::{collections::BTreeMap, fmt::Debug};

use k8s_openapi::{NamespaceResourceScope, api::core::v1::Secret};
use kube::{CustomResourceExt, Resource, ResourceExt, api::ObjectMeta};

use crate::svc::k8s::resource;

// -----------------------------------------------------------------------------
// Constants

pub const OVERRIDE_CONFIGURATION_NAME: &str = "clever-kubernetes-operator";

// -----------------------------------------------------------------------------
// Helpers

#[cfg_attr(feature = "tracing", tracing::instrument)]
pub fn name<T>(obj: &T) -> String
where
    T: Resource<Scope = NamespaceResourceScope> + ResourceExt + Debug,
{
    format!("{}-secrets", obj.name_any())
}

#[cfg_attr(feature = "tracing", tracing::instrument)]
pub fn new<T>(obj: &T, secrets: BTreeMap<String, String>) -> Secret
where
    T: Resource<Scope = NamespaceResourceScope> + ResourceExt + CustomResourceExt + Debug,
{
    let owner = resource::owner_reference(obj);
    let metadata = ObjectMeta {
        name: Some(name(obj)),
        namespace: obj.namespace(),
        owner_references: Some(vec![owner]),
        ..Default::default()
    };

    Secret {
        metadata,
        string_data: Some(secrets),
        ..Default::default()
    }
}
