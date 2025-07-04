//! Configuration schema.

use std::{
    any,
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    iter,
};

use anyhow::Context;

use self::mount::{MountingPoint, MountingPoints};
use crate::{
    metadata::{
        AliasOptions, BasicTypes, ConfigMetadata, ConfigVariant, NestedConfigMetadata,
        ParamMetadata,
    },
    utils::EnumVariant,
    value::Pointer,
};

mod mount;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy)]
struct ParentLink {
    parent_ty: any::TypeId,
    this_ref: &'static NestedConfigMetadata,
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigData {
    pub(crate) metadata: &'static ConfigMetadata,
    parent_link: Option<ParentLink>,
    pub(crate) is_top_level: bool,
    pub(crate) coerce_serde_enums: bool,
    all_paths: Vec<(Cow<'static, str>, AliasOptions)>,
}

impl ConfigData {
    pub(crate) fn prefix(&self) -> Pointer<'_> {
        Pointer(self.all_paths[0].0.as_ref())
    }

    pub(crate) fn aliases(&self) -> impl Iterator<Item = (&str, AliasOptions)> + '_ {
        self.all_paths
            .iter()
            .skip(1)
            .map(|(path, options)| (path.as_ref(), *options))
    }

    pub(crate) fn all_paths_for_param(
        &self,
        param: &'static ParamMetadata,
    ) -> impl Iterator<Item = (String, AliasOptions)> + '_ {
        self.all_paths_for_child(param.name, param.aliases, param.tag_variant)
    }

    fn all_paths_for_child(
        &self,
        name: &'static str,
        aliases: &'static [(&'static str, AliasOptions)],
        tag_variant: Option<&'static ConfigVariant>,
    ) -> impl Iterator<Item = (String, AliasOptions)> + '_ {
        let local_names =
            iter::once((name, AliasOptions::default())).chain(aliases.iter().copied());

        let enum_names = if let (true, Some(variant)) = (self.coerce_serde_enums, tag_variant) {
            let variant_names = iter::once(variant.name)
                .chain(variant.aliases.iter().copied())
                .filter_map(|name| Some(EnumVariant::new(name)?.to_snake_case()));
            let local_names_ = local_names.clone();
            let paths = variant_names.flat_map(move |variant_name| {
                local_names_
                    .clone()
                    .filter_map(move |(name_or_path, options)| {
                        if name_or_path.starts_with('.') {
                            // Only consider simple aliases, not path ones.
                            return None;
                        }
                        let full_path = Pointer(&variant_name).join(name_or_path);
                        Some((Cow::Owned(full_path), options))
                    })
            });
            Some(paths)
        } else {
            None
        };
        let enum_names = enum_names.into_iter().flatten();
        let local_names = local_names
            .map(|(name, options)| (Cow::Borrowed(name), options))
            .chain(enum_names);

        self.all_paths
            .iter()
            .flat_map(move |(alias, config_options)| {
                local_names
                    .clone()
                    .filter_map(move |(name_or_path, options)| {
                        let full_path = Pointer(alias).join_path(Pointer(&name_or_path))?;
                        Some((full_path, options.combine(*config_options)))
                    })
            })
    }
}

/// Reference to a specific configuration inside [`ConfigSchema`].
#[derive(Debug, Clone, Copy)]
pub struct ConfigRef<'a> {
    schema: &'a ConfigSchema,
    prefix: &'a str,
    pub(crate) data: &'a ConfigData,
}

impl<'a> ConfigRef<'a> {
    /// Gets the config prefix.
    pub fn prefix(&self) -> &'a str {
        self.prefix
    }

    /// Gets the config metadata.
    pub fn metadata(&self) -> &'static ConfigMetadata {
        self.data.metadata
    }

    /// Checks whether this config is top-level (i.e., was included into the schema directly, rather than as a sub-config).
    pub fn is_top_level(&self) -> bool {
        self.data.parent_link.is_none()
    }

    #[doc(hidden)] // not stabilized yet
    pub fn parent_link(&self) -> Option<(Self, &'static NestedConfigMetadata)> {
        let link = self.data.parent_link?;
        let parent_prefix = if link.this_ref.name.is_empty() {
            // Flattened config
            self.prefix
        } else {
            let (parent, _) = Pointer(self.prefix).split_last().unwrap();
            parent.0
        };
        let parent_ref = Self {
            schema: self.schema,
            prefix: parent_prefix,
            data: self.schema.get_ll(parent_prefix, link.parent_ty)?,
        };
        Some((parent_ref, link.this_ref))
    }

    /// Iterates over all aliases for this config.
    pub fn aliases(&self) -> impl Iterator<Item = (&'a str, AliasOptions)> + '_ {
        self.data.aliases()
    }

    /// Returns a prioritized list of absolute paths to the specified param (higher-priority paths first).
    /// For the result to make sense, the param must be a part of this config.
    #[doc(hidden)] // too low-level
    pub fn all_paths_for_param(
        &self,
        param: &'static ParamMetadata,
    ) -> impl Iterator<Item = (String, AliasOptions)> + '_ {
        self.data.all_paths_for_param(param)
    }
}

/// Mutable reference to a specific configuration inside [`ConfigSchema`].
#[derive(Debug)]
pub struct ConfigMut<'a> {
    schema: &'a mut ConfigSchema,
    prefix: String,
    type_id: any::TypeId,
}

impl ConfigMut<'_> {
    /// Gets the config prefix.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Iterates over all aliases for this config.
    pub fn aliases(&self) -> impl Iterator<Item = (&str, AliasOptions)> + '_ {
        let data = &self.schema.configs[self.prefix.as_str()].inner[&self.type_id];
        data.aliases()
    }

    /// Pushes an additional alias for the config.
    ///
    /// # Errors
    ///
    /// Returns an error if adding a config leads to violations of fundamental invariants
    /// (same as for [`ConfigSchema::insert()`]).
    pub fn push_alias(self, alias: &'static str) -> anyhow::Result<Self> {
        self.push_alias_inner(alias, AliasOptions::new())
    }

    /// Same as [`Self::push_alias()`], but also marks the alias as deprecated.
    ///
    /// # Errors
    ///
    /// Returns an error if adding a config leads to violations of fundamental invariants
    /// (same as for [`ConfigSchema::insert()`]).
    pub fn push_deprecated_alias(self, alias: &'static str) -> anyhow::Result<Self> {
        self.push_alias_inner(
            alias,
            AliasOptions {
                is_deprecated: true,
            },
        )
    }

    fn push_alias_inner(self, alias: &'static str, options: AliasOptions) -> anyhow::Result<Self> {
        let mut patched = PatchedSchema::new(self.schema);
        patched.insert_alias(self.prefix.clone(), self.type_id, Pointer(alias), options)?;
        patched.commit();
        Ok(self)
    }
}

#[derive(Debug, Clone, Default)]
struct ConfigsForPrefix {
    inner: HashMap<any::TypeId, ConfigData>,
    by_depth: BTreeSet<(usize, any::TypeId)>,
}

impl ConfigsForPrefix {
    fn by_depth(&self) -> impl Iterator<Item = &ConfigData> + '_ {
        self.by_depth.iter().map(|(_, ty)| &self.inner[ty])
    }

    fn insert(&mut self, ty: any::TypeId, depth: Option<usize>, data: ConfigData) {
        self.inner.insert(ty, data);
        if let Some(depth) = depth {
            self.by_depth.insert((depth, ty));
        }
    }

    fn extend(&mut self, other: Self) {
        self.inner.extend(other.inner);
        self.by_depth.extend(other.by_depth);
    }
}

/// Schema for configuration. Can contain multiple configs bound to different paths.
// TODO: more docs; e.g., document global aliases
#[derive(Debug, Clone, Default)]
pub struct ConfigSchema {
    // Order configs by canonical prefix for iteration etc. Also, this makes configs iterator topologically
    // sorted, and makes it easy to query prefix ranges, but these properties aren't used for now.
    configs: BTreeMap<Cow<'static, str>, ConfigsForPrefix>,
    mounting_points: MountingPoints,
    coerce_serde_enums: bool,
}

impl ConfigSchema {
    /// Creates a schema consisting of a single configuration at the specified prefix.
    #[allow(clippy::missing_panics_doc)]
    pub fn new(metadata: &'static ConfigMetadata, prefix: &'static str) -> Self {
        let mut this = Self::default();
        this.insert(metadata, prefix)
            .expect("internal error: failed inserting first config to the schema");
        this
    }

    /// Switches coercing for serde-like enums. Coercion will add path aliases for all tagged params in enum configs
    /// added to the schema afterward (or until `coerce_serde_enums(false)` is called). Coercion will apply
    /// to nested enum configs as well.
    ///
    /// For example, if a config param named `param` corresponds to the tag `SomeTag`, then alias `.some_tag.param`
    /// (`snake_cased` tag + param name) will be added for the param. Tag aliases and param aliases will result
    /// in additional path aliases, as expected. For example, if `param` has alias `alias` and the tag has alias `AliasTag`,
    /// then the param will have `.alias_tag.param`, `.alias_tag.alias` and `.some_tag.alias` aliases.
    pub fn coerce_serde_enums(&mut self, coerce: bool) -> &mut Self {
        self.coerce_serde_enums = coerce;
        self
    }

    /// Iterates over all configs with their canonical prefixes.
    pub(crate) fn iter_ll(&self) -> impl Iterator<Item = (Pointer<'_>, &ConfigData)> + '_ {
        self.configs
            .iter()
            .flat_map(|(prefix, data)| data.inner.values().map(move |data| (Pointer(prefix), data)))
    }

    pub(crate) fn contains_canonical_param(&self, at: Pointer<'_>) -> bool {
        self.mounting_points.get(at.0).is_some_and(|mount| {
            matches!(
                mount,
                MountingPoint::Param {
                    is_canonical: true,
                    ..
                }
            )
        })
    }

    pub(crate) fn params_with_kv_path<'s>(
        &'s self,
        kv_path: &'s str,
    ) -> impl Iterator<Item = (Pointer<'s>, BasicTypes)> + 's {
        self.mounting_points
            .by_kv_path(kv_path)
            .filter_map(|(path, mount)| {
                let expecting = match mount {
                    MountingPoint::Param { expecting, .. } => *expecting,
                    MountingPoint::Config => return None,
                };
                Some((path, expecting))
            })
    }

    /// Iterates over all configs contained in this schema. A unique key for a config is its type + location;
    /// i.e., multiple returned refs may have the same config type xor same location (never both).
    pub fn iter(&self) -> impl Iterator<Item = ConfigRef<'_>> + '_ {
        self.configs.iter().flat_map(move |(prefix, data)| {
            data.by_depth().map(move |data| ConfigRef {
                schema: self,
                prefix: prefix.as_ref(),
                data,
            })
        })
    }

    /// Lists all prefixes for the specified config. This does not include aliases.
    pub fn locate(&self, metadata: &'static ConfigMetadata) -> impl Iterator<Item = &str> + '_ {
        let config_type_id = metadata.ty.id();
        self.configs.iter().filter_map(move |(prefix, data)| {
            data.inner
                .contains_key(&config_type_id)
                .then_some(prefix.as_ref())
        })
    }

    /// Gets a reference to a config by ist unique key (metadata + canonical prefix).
    pub fn get<'s>(
        &'s self,
        metadata: &'static ConfigMetadata,
        prefix: &'s str,
    ) -> Option<ConfigRef<'s>> {
        let data = self.get_ll(prefix, metadata.ty.id())?;
        Some(ConfigRef {
            schema: self,
            prefix,
            data,
        })
    }

    fn get_ll(&self, prefix: &str, ty: any::TypeId) -> Option<&ConfigData> {
        self.configs.get(prefix)?.inner.get(&ty)
    }

    /// Gets a reference to a config by ist unique key (metadata + canonical prefix).
    pub fn get_mut(
        &mut self,
        metadata: &'static ConfigMetadata,
        prefix: &str,
    ) -> Option<ConfigMut<'_>> {
        let ty = metadata.ty.id();
        if !self.configs.get(prefix)?.inner.contains_key(&ty) {
            return None;
        }

        Some(ConfigMut {
            schema: self,
            prefix: prefix.to_owned(),
            type_id: ty,
        })
    }

    /// Returns a single reference to the specified config.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration is not registered or has more than one mount point.
    #[allow(clippy::missing_panics_doc)] // false positive
    pub fn single(&self, metadata: &'static ConfigMetadata) -> anyhow::Result<ConfigRef<'_>> {
        let prefixes: Vec<_> = self.locate(metadata).take(2).collect();
        match prefixes.as_slice() {
            [] => anyhow::bail!(
                "configuration `{}` is not registered in schema",
                metadata.ty.name_in_code()
            ),
            &[prefix] => Ok(ConfigRef {
                schema: self,
                prefix,
                data: &self.configs[prefix].inner[&metadata.ty.id()],
            }),
            [first, second] => anyhow::bail!(
                "configuration `{}` is registered in at least 2 locations: {first:?}, {second:?}",
                metadata.ty.name_in_code()
            ),
            _ => unreachable!(),
        }
    }

    /// Returns a single mutable reference to the specified config.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration is not registered or has more than one mount point.
    #[allow(clippy::missing_panics_doc)] // false positive
    pub fn single_mut(
        &mut self,
        metadata: &'static ConfigMetadata,
    ) -> anyhow::Result<ConfigMut<'_>> {
        let mut it = self.locate(metadata);
        let first_prefix = it.next().with_context(|| {
            format!(
                "configuration `{}` is not registered in schema",
                metadata.ty.name_in_code()
            )
        })?;
        if let Some(second_prefix) = it.next() {
            anyhow::bail!(
                "configuration `{}` is registered in at least 2 locations: {first_prefix:?}, {second_prefix:?}",
                metadata.ty.name_in_code()
            );
        }

        drop(it);
        let prefix = first_prefix.to_owned();
        Ok(ConfigMut {
            schema: self,
            type_id: metadata.ty.id(),
            prefix,
        })
    }

    /// Inserts a new configuration type at the specified place.
    ///
    /// # Errors
    ///
    /// Returns an error if adding a config leads to violations of fundamental invariants:
    ///
    /// - If a parameter in the new config (taking aliases into account, and params in nested / flattened configs)
    ///   is mounted at the location of an existing config.
    /// - Vice versa, if a config or nested config is mounted at the location of an existing param.
    /// - If a parameter is mounted at the location of a parameter with disjoint [expected types](ParamMetadata.expecting).
    pub fn insert(
        &mut self,
        metadata: &'static ConfigMetadata,
        prefix: &'static str,
    ) -> anyhow::Result<ConfigMut<'_>> {
        let coerce_serde_enums = self.coerce_serde_enums;
        let mut patched = PatchedSchema::new(self);
        patched.insert_config(prefix, metadata, coerce_serde_enums)?;
        patched.commit();
        Ok(ConfigMut {
            schema: self,
            type_id: metadata.ty.id(),
            prefix: prefix.to_owned(),
        })
    }
}

/// [`ConfigSchema`] together with a patch that can be atomically committed.
#[derive(Debug)]
#[must_use = "Should be `commit()`ted"]
struct PatchedSchema<'a> {
    base: &'a mut ConfigSchema,
    patch: ConfigSchema,
}

impl<'a> PatchedSchema<'a> {
    fn new(base: &'a mut ConfigSchema) -> Self {
        Self {
            base,
            patch: ConfigSchema::default(),
        }
    }

    fn mount(&self, path: &str) -> Option<&MountingPoint> {
        self.patch
            .mounting_points
            .get(path)
            .or_else(|| self.base.mounting_points.get(path))
    }

    fn insert_config(
        &mut self,
        prefix: &'static str,
        metadata: &'static ConfigMetadata,
        coerce_serde_enums: bool,
    ) -> anyhow::Result<()> {
        self.insert_recursively(
            prefix.into(),
            true,
            ConfigData {
                metadata,
                parent_link: None,
                is_top_level: true,
                coerce_serde_enums,
                all_paths: vec![(prefix.into(), AliasOptions::new())],
            },
        )
    }

    fn insert_recursively(
        &mut self,
        prefix: Cow<'static, str>,
        is_new: bool,
        data: ConfigData,
    ) -> anyhow::Result<()> {
        let depth = is_new.then_some(0_usize);
        let mut pending_configs = vec![(prefix, data, depth)];

        // Insert / update all nested configs recursively.
        while let Some((prefix, data, depth)) = pending_configs.pop() {
            // Check whether the config is already present; if so, no need to insert the config
            // or any nested configs.
            if is_new && self.base.get_ll(&prefix, data.metadata.ty.id()).is_some() {
                continue;
            }

            let child_depth = depth.map(|d| d + 1);
            let new_configs = Self::list_nested_configs(Pointer(&prefix), &data)
                .map(|(prefix, data)| (prefix.into(), data, child_depth));
            pending_configs.extend(new_configs);
            self.insert_inner(prefix, depth, data)?;
        }
        Ok(())
    }

    fn insert_alias(
        &mut self,
        prefix: String,
        config_id: any::TypeId,
        alias: Pointer<'static>,
        options: AliasOptions,
    ) -> anyhow::Result<()> {
        let config_data = &self.base.configs[prefix.as_str()].inner[&config_id];
        if config_data
            .all_paths
            .iter()
            .any(|(name, _)| name == alias.0)
        {
            return Ok(()); // shortcut in the no-op case
        }

        let metadata = config_data.metadata;
        self.insert_recursively(
            prefix.into(),
            false,
            ConfigData {
                metadata,
                parent_link: config_data.parent_link,
                is_top_level: config_data.is_top_level,
                coerce_serde_enums: config_data.coerce_serde_enums,
                all_paths: vec![(alias.0.into(), options)],
            },
        )
    }

    fn list_nested_configs<'i>(
        prefix: Pointer<'i>,
        data: &'i ConfigData,
    ) -> impl Iterator<Item = (String, ConfigData)> + 'i {
        data.metadata.nested_configs.iter().map(move |nested| {
            let all_paths =
                data.all_paths_for_child(nested.name, nested.aliases, nested.tag_variant);
            let all_paths = all_paths
                .map(|(path, options)| (Cow::Owned(path), options))
                .collect();

            let config_data = ConfigData {
                metadata: nested.meta,
                parent_link: Some(ParentLink {
                    parent_ty: data.metadata.ty.id(),
                    this_ref: nested,
                }),
                is_top_level: false,
                coerce_serde_enums: data.coerce_serde_enums,
                all_paths,
            };
            (prefix.join(nested.name), config_data)
        })
    }

    fn insert_inner(
        &mut self,
        prefix: Cow<'static, str>,
        depth: Option<usize>,
        mut data: ConfigData,
    ) -> anyhow::Result<()> {
        let config_name = data.metadata.ty.name_in_code();
        let config_paths = data.all_paths.iter().map(|(name, _)| name.as_ref());
        let config_paths = iter::once(prefix.as_ref()).chain(config_paths);

        for path in config_paths {
            if let Some(mount) = self.mount(path) {
                match mount {
                    MountingPoint::Config => { /* OK */ }
                    MountingPoint::Param { .. } => {
                        anyhow::bail!(
                            "Cannot mount config `{}` at `{path}` because parameter(s) are already mounted at this path",
                            data.metadata.ty.name_in_code()
                        );
                    }
                }
            }
            self.patch
                .mounting_points
                .insert(path.to_owned(), MountingPoint::Config);
        }

        for param in data.metadata.params {
            let all_paths = data.all_paths_for_param(param);

            for (name_i, (full_name, _)) in all_paths.enumerate() {
                let mut was_canonical = false;
                if let Some(mount) = self.mount(&full_name) {
                    let prev_expecting = match mount {
                        MountingPoint::Param {
                            expecting,
                            is_canonical,
                        } => {
                            was_canonical = *is_canonical;
                            *expecting
                        }
                        MountingPoint::Config => {
                            anyhow::bail!(
                                "Cannot insert param `{name}` [Rust field: `{field}`] from config `{config_name}` at `{full_name}`: \
                                 config(s) are already mounted at this path",
                                name = param.name,
                                field = param.rust_field_name
                            );
                        }
                    };

                    if prev_expecting != param.expecting {
                        anyhow::bail!(
                            "Cannot insert param `{name}` [Rust field: `{field}`] from config `{config_name}` at `{full_name}`: \
                             it expects {expecting}, while the existing param(s) mounted at this path expect {prev_expecting}",
                            name = param.name,
                            field = param.rust_field_name,
                            expecting = param.expecting
                        );
                    }
                }
                let is_canonical = was_canonical || name_i == 0;
                self.patch.mounting_points.insert(
                    full_name,
                    MountingPoint::Param {
                        expecting: param.expecting,
                        is_canonical,
                    },
                );
            }
        }

        // `data` is the new data for the config, so we need to consult `base` for existing data.
        // Unlike with params, by design we never insert same config entries in the same patch,
        // so it's safe to *only* consult `base`.
        let config_id = data.metadata.ty.id();
        let prev_data = self.base.get_ll(&prefix, config_id);
        if let Some(prev_data) = prev_data {
            // Append new aliases to the end since their ordering determines alias priority
            let mut all_paths = prev_data.all_paths.clone();
            all_paths.extend_from_slice(&data.all_paths);
            data.all_paths = all_paths;
        }

        self.patch
            .configs
            .entry(prefix)
            .or_default()
            .insert(config_id, depth, data);
        Ok(())
    }

    fn commit(self) {
        for (prefix, data) in self.patch.configs {
            let prev_data = self.base.configs.entry(prefix).or_default();
            prev_data.extend(data);
        }
        self.base.mounting_points.extend(self.patch.mounting_points);
    }
}
