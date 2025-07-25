// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Oso integration

use super::Authz;
use super::actor::AnyActor;
use super::actor::AuthenticatedActor;
use super::api_resources::*;
use super::context::AuthorizedResource;
use super::roles::RoleSet;
use crate::authn;
use crate::context::OpContext;
use anyhow::Context;
use anyhow::ensure;
use futures::FutureExt;
use futures::future::BoxFuture;
use omicron_common::api::external::Error;
use oso::Oso;
use oso::PolarClass;
use slog::info;
use std::collections::BTreeSet;
use std::fmt;

/// Base Polar configuration describing control plane authorization rules
const OMICRON_AUTHZ_CONFIG_BASE: &str = include_str!("omicron.polar");

/// Used to configure Polar resources
pub(super) struct Init {
    /// snippet of Polar describing a resource
    pub polar_snippet: &'static str,
    /// description of Rust type implementing the Polar resource
    pub polar_class: oso::Class,
}

/// Represents an initialized Oso environment
pub struct OsoInit {
    pub oso: Oso,
    pub class_names: BTreeSet<String>,
}

/// Manages initialization of Oso environment, including Polar snippets and
/// Polar classes
pub struct OsoInitBuilder {
    log: slog::Logger,
    oso: Oso,
    class_names: BTreeSet<String>,
    snippets: Vec<&'static str>,
}

impl OsoInitBuilder {
    pub fn new(log: slog::Logger) -> OsoInitBuilder {
        OsoInitBuilder {
            log,
            oso: Oso::new(),
            class_names: BTreeSet::new(),
            snippets: vec![OMICRON_AUTHZ_CONFIG_BASE],
        }
    }

    /// Registers a class that either has no associated Polar snippet or whose
    /// snippet is part of the base file
    pub fn register_class(
        mut self,
        c: oso::Class,
    ) -> Result<Self, anyhow::Error> {
        info!(self.log, "registering Oso class"; "class" => &c.name);
        let name = c.name.clone();
        let new_element = self.class_names.insert(name.clone());
        ensure!(new_element, "Oso class was already registered: {:?}", &name);
        self.oso
            .register_class(c)
            .with_context(|| format!("registering Oso class {:?}", name))?;
        Ok(self)
    }

    /// Registers a class with associated Polar snippet
    pub(super) fn register_class_with_snippet(
        mut self,
        init: Init,
    ) -> Result<Self, anyhow::Error> {
        self.snippets.push(init.polar_snippet);
        self.register_class(init.polar_class)
    }

    pub fn build(mut self) -> Result<OsoInit, anyhow::Error> {
        let polar_config = self.snippets.join("\n");
        info!(self.log, "full Oso configuration"; "config" => &polar_config);
        self.oso
            .load_str(&polar_config)
            .context("loading Polar (Oso) config")?;
        Ok(OsoInit { oso: self.oso, class_names: self.class_names })
    }
}

/// Returns an Oso handle suitable for authorizing using Omicron's authorization
/// rules
pub fn make_omicron_oso(log: &slog::Logger) -> Result<OsoInit, anyhow::Error> {
    let mut oso_builder = OsoInitBuilder::new(log.clone());
    let classes = [
        // Hand-written classes
        Action::get_polar_class(),
        AnyActor::get_polar_class(),
        AuthenticatedActor::get_polar_class(),
        BlueprintConfig::get_polar_class(),
        Database::get_polar_class(),
        DnsConfig::get_polar_class(),
        Fleet::get_polar_class(),
        Inventory::get_polar_class(),
        IpPoolList::get_polar_class(),
        ConsoleSessionList::get_polar_class(),
        DeviceAuthRequestList::get_polar_class(),
        SiloCertificateList::get_polar_class(),
        SiloIdentityProviderList::get_polar_class(),
        SiloUserList::get_polar_class(),
        UpdateTrustRootList::get_polar_class(),
        TargetReleaseConfig::get_polar_class(),
        AlertClassList::get_polar_class(),
    ];
    for c in classes {
        oso_builder = oso_builder.register_class(c)?;
    }

    // Generated by the `authz_resource!` macro
    let generated_inits = [
        Project::init(),
        Disk::init(),
        Snapshot::init(),
        ProjectImage::init(),
        AffinityGroup::init(),
        AntiAffinityGroup::init(),
        Instance::init(),
        IpPool::init(),
        InstanceNetworkInterface::init(),
        Vpc::init(),
        VpcRouter::init(),
        InternetGateway::init(),
        InternetGatewayIpPool::init(),
        InternetGatewayIpAddress::init(),
        RouterRoute::init(),
        VpcSubnet::init(),
        FloatingIp::init(),
        // Silo-level resources
        Image::init(),
        SiloImage::init(),
        // Fleet-level resources
        AddressLot::init(),
        Blueprint::init(),
        LoopbackAddress::init(),
        Certificate::init(),
        ConsoleSession::init(),
        DeviceAuthRequest::init(),
        DeviceAccessToken::init(),
        PhysicalDisk::init(),
        Rack::init(),
        SshKey::init(),
        Silo::init(),
        SiloUser::init(),
        SiloGroup::init(),
        SupportBundle::init(),
        IdentityProvider::init(),
        SamlIdentityProvider::init(),
        Sled::init(),
        TufRepo::init(),
        TufArtifact::init(),
        TufTrustRoot::init(),
        Alert::init(),
        AlertReceiver::init(),
        WebhookSecret::init(),
        Zpool::init(),
        Service::init(),
        UserBuiltin::init(),
    ];

    for init in generated_inits {
        oso_builder = oso_builder.register_class_with_snippet(init)?;
    }

    oso_builder.build()
}

/// Describes an action being authorized
///
/// There's currently just one enum of Actions for all of Omicron.  We expect
/// most objects to support mostly the same set of actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::EnumIter)]
pub enum Action {
    Query, // only used for `Database`
    Read,
    ListChildren,
    ReadPolicy,
    Modify,
    ModifyPolicy,
    CreateChild,
    Delete,
}

impl oso::PolarClass for Action {
    fn get_polar_class_builder() -> oso::ClassBuilder<Self> {
        oso::Class::builder()
            .with_equality_check()
            .add_method("to_perm", |a: &Action| Perm::from(a).to_string())
    }
}

/// A permission used in the Polar configuration
///
/// An authorization request starts by asking whether an actor can take some
/// _action_ on a resource.  Most of the policy is written in terms of
/// traditional RBAC-style _permissions_.  This type is used to help translate
/// from [`Action`] to permission.
///
/// Note that Polar appears to require that all permissions be strings.  So in
/// practice, the [`Action`] is converted to a [`Perm`] only for long enough to
/// convert that to a string.  Still, having a separate type here ensures that
/// not _any_ old string can be used as a permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Perm {
    Query, // Only for `Database`
    Read,
    Modify,
    ListChildren,
    CreateChild,
}

impl From<&Action> for Perm {
    fn from(a: &Action) -> Self {
        match a {
            Action::Query => Perm::Query,
            Action::Read => Perm::Read,
            Action::ReadPolicy => Perm::Read,
            Action::Modify => Perm::Modify,
            Action::ModifyPolicy => Perm::Modify,
            Action::Delete => Perm::Modify,
            Action::ListChildren => Perm::ListChildren,
            Action::CreateChild => Perm::CreateChild,
        }
    }
}

impl fmt::Display for Perm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // This implementation MUST be kept in sync with the Polar configuration
        // for Omicron, which uses literal strings for permissions.
        f.write_str(match self {
            Perm::Query => "query",
            Perm::Read => "read",
            Perm::Modify => "modify",
            Perm::ListChildren => "list_children",
            Perm::CreateChild => "create_child",
        })
    }
}

// Non-API resources that we want to protect with authorization

/// Represents the database itself to Polar
///
/// This exists so that we can have roles with no access to the database at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Database;
/// Singleton representing the `Database` itself for authz purposes
pub const DATABASE: Database = Database;

impl oso::PolarClass for Database {
    fn get_polar_class_builder() -> oso::ClassBuilder<Self> {
        oso::Class::builder().add_method(
            "has_role",
            |_d: &Database, _actor: AuthenticatedActor, _role: String| {
                // There is an explicit rule in the Oso policy granting the
                // appropriate roles on "Database" to the appropriate actors.
                // We don't need to grant anything extra here.
                false
            },
        )
    }
}

impl AuthorizedResource for Database {
    fn load_roles<'fut>(
        &'fut self,
        _: &'fut OpContext,
        _: &'fut authn::Context,
        _: &'fut mut RoleSet,
    ) -> BoxFuture<'fut, Result<(), Error>> {
        // We don't use (database) roles to grant access to the database.  The
        // role assignment is hardcoded for all authenticated users.  See the
        // "has_role" Polar method above.
        //
        // Instead of this, we could modify this function to insert into
        // `RoleSet` the "database user" role.  However, this doesn't fit into
        // the type signature of roles supported by RoleSet.  RoleSet is really
        // for roles on database objects -- it assumes they have a ResourceType
        // and id, neither of which is true for `Database`.
        futures::future::ready(Ok(())).boxed()
    }

    fn on_unauthorized(
        &self,
        _: &Authz,
        error: Error,
        _: AnyActor,
        _: Action,
    ) -> Error {
        error
    }

    fn polar_class(&self) -> oso::Class {
        Self::get_polar_class()
    }
}

#[cfg(test)]
mod test {
    use super::OsoInitBuilder;
    use crate::authz::Action;
    use omicron_test_utils::dev;
    use oso::PolarClass;

    #[test]
    fn test_duplicate_polar_classes() {
        let logctx = dev::test_setup_log("test_duplicate_polar_classes");
        let oso_builder = OsoInitBuilder::new(logctx.log.clone());
        let oso_builder =
            oso_builder.register_class(Action::get_polar_class()).unwrap();
        match oso_builder.register_class(Action::get_polar_class()) {
            Ok(_) => panic!("failed to detect duplicate class"),
            Err(error) => {
                println!("{:#}", error);
                assert_eq!(
                    error.to_string(),
                    "Oso class was already registered: \"Action\""
                );
            }
        };
        logctx.cleanup_successful();
    }
}
