// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Renders the CRDs from the Rust types (ADR-0003). `make crds` writes them to
//! `deploy/crds/<group>_<plural>.yaml`; a test fails when the checked-in files are stale.

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceDefinition, JSONSchemaProps, JSONSchemaPropsOrArray, JSONSchemaPropsOrBool,
    JSONSchemaPropsOrStringArray,
};
use kube::CustomResourceExt;

use crate::host::Host;
use crate::zone::AvailabilityZone;

/// Every CRD faubourg serves, with the formats the apiserver would drop removed
/// ([`strip_unsupported_formats`]).
pub fn crds() -> Vec<CustomResourceDefinition> {
    let mut crds = vec![AvailabilityZone::crd(), Host::crd()];
    crds.iter_mut().for_each(strip_unsupported_formats);
    crds
}

/// Integer formats the apiserver keeps (`type: integer`).
const INTEGER_FORMATS: [&str; 2] = ["int32", "int64"];

/// Number formats the apiserver keeps (`type: number`).
const NUMBER_FORMATS: [&str; 2] = ["float", "double"];

/// String formats the apiserver keeps, without dashes: it compares formats with their
/// dashes removed, so `date-time` matches `datetime`. Mirrors `supportedVersionedFormats`
/// in apiextensions-apiserver's `pkg/apiserver/validation/formats.go`.
const STRING_FORMATS: [&str; 26] = [
    "bsonobjectid",
    "uri",
    "email",
    "hostname",
    "ipv4",
    "ipv6",
    "cidr",
    "mac",
    "uuid",
    "uuid3",
    "uuid4",
    "uuid5",
    "isbn",
    "isbn10",
    "isbn13",
    "creditcard",
    "ssn",
    "hexcolor",
    "rgbcolor",
    "byte",
    "password",
    "date",
    "duration",
    "datetime",
    "k8sshortname",
    "k8slongname",
];

/// Remove every `format` the apiserver does not recognise from each version's schema.
///
/// `schemars` names Rust's unsigned integers `uint8` to `uint64`. Kubernetes knows none of
/// them: it drops them from the stored schema, so they validate nothing, and warns on apply
/// (`unrecognized format "uint32"`). The bounds that matter (`minimum`, `maximum`) are
/// separate keywords and stay.
pub fn strip_unsupported_formats(crd: &mut CustomResourceDefinition) {
    for version in &mut crd.spec.versions {
        if let Some(root) = version
            .schema
            .as_mut()
            .and_then(|s| s.open_api_v3_schema.as_mut())
        {
            strip_unsupported_schema_formats(root);
        }
    }
}

/// Whether the apiserver keeps `format` on a `type: string` (or untyped) schema. Dashes
/// are ignored, as the apiserver ignores them.
#[must_use]
pub fn is_supported_string_format(format: &str) -> bool {
    STRING_FORMATS.contains(&format.replace('-', "").as_str())
}

/// [`strip_unsupported_formats`] for one schema and everything under it: properties,
/// items, additional properties and items, the combinators, pattern properties,
/// definitions and schema dependencies.
pub fn strip_unsupported_schema_formats(schema: &mut JSONSchemaProps) {
    if let Some(format) = schema.format.as_deref() {
        let keep = match schema.type_.as_deref() {
            Some("integer") => INTEGER_FORMATS.contains(&format),
            Some("number") => NUMBER_FORMATS.contains(&format),
            None | Some("string") => is_supported_string_format(format),
            Some(_) => false,
        };
        if !keep {
            schema.format = None;
        }
    }

    let maps = [
        schema.properties.as_mut(),
        schema.pattern_properties.as_mut(),
        schema.definitions.as_mut(),
    ];
    for child in maps.into_iter().flatten().flat_map(|m| m.values_mut()) {
        strip_unsupported_schema_formats(child);
    }
    let lists = [
        schema.all_of.as_mut(),
        schema.any_of.as_mut(),
        schema.one_of.as_mut(),
    ];
    for child in lists.into_iter().flatten().flat_map(|v| v.iter_mut()) {
        strip_unsupported_schema_formats(child);
    }
    match schema.items.as_mut() {
        Some(JSONSchemaPropsOrArray::Schema(child)) => strip_unsupported_schema_formats(child),
        Some(JSONSchemaPropsOrArray::Schemas(children)) => {
            children
                .iter_mut()
                .for_each(strip_unsupported_schema_formats);
        }
        None => {}
    }
    for child in [
        schema.additional_properties.as_mut(),
        schema.additional_items.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        if let JSONSchemaPropsOrBool::Schema(child) = child {
            strip_unsupported_schema_formats(child);
        }
    }
    if let Some(child) = schema.not.as_mut() {
        strip_unsupported_schema_formats(child);
    }
    for dependency in schema.dependencies.iter_mut().flat_map(|m| m.values_mut()) {
        if let JSONSchemaPropsOrStringArray::Schema(child) = dependency {
            strip_unsupported_schema_formats(child);
        }
    }
}

/// The kubebuilder file name for a CRD: `<group>_<plural>.yaml`.
pub fn file_name(crd: &CustomResourceDefinition) -> String {
    format!("{}_{}.yaml", crd.spec.group, crd.spec.names.plural)
}

/// One CRD as YAML, with the license header every tracked file carries.
pub fn render(crd: &CustomResourceDefinition) -> Result<String, serde_saphyr::ser::Error> {
    let body = serde_saphyr::to_string(crd)?;
    Ok(format!(
        "# Copyright (c) 2026 Erick Bourgeois, faubourg\n\
         # SPDX-License-Identifier: Apache-2.0\n\
         # Generated by `make crds` from crates/faubourg-api. Do not edit.\n\
         ---\n{body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// Every `format` in `schema` and under it, with the `type` beside it.
    fn formats(schema: &serde_json::Value, path: &str, out: &mut Vec<String>) {
        match schema {
            serde_json::Value::Object(map) => {
                if let (Some(serde_json::Value::String(f)), ty) =
                    (map.get("format"), map.get("type"))
                {
                    out.push(format!("{path}: {f} ({ty:?})"));
                }
                for (k, v) in map {
                    formats(v, &format!("{path}.{k}"), out);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    formats(v, &format!("{path}[{i}]"), out);
                }
            }
            _ => {}
        }
    }

    fn props(type_: &str, format: &str) -> JSONSchemaProps {
        JSONSchemaProps {
            type_: Some(type_.to_string()),
            format: Some(format.to_string()),
            ..JSONSchemaProps::default()
        }
    }

    fn kept(type_: &str, format: &str) -> bool {
        let mut schema = props(type_, format);
        strip_unsupported_schema_formats(&mut schema);
        schema.format.is_some()
    }

    #[test]
    fn integer_formats_keep_only_int32_and_int64() {
        assert!(kept("integer", "int32"));
        assert!(kept("integer", "int64"));
        for f in ["uint8", "uint16", "uint32", "uint64", "double"] {
            assert!(!kept("integer", f), "{f}");
        }
    }

    #[test]
    fn number_formats_keep_only_float_and_double() {
        assert!(kept("number", "float"));
        assert!(kept("number", "double"));
        assert!(!kept("number", "int64"));
    }

    #[test]
    fn string_formats_keep_the_kubernetes_set() {
        for f in [
            "date-time",
            "datetime",
            "uuid",
            "ipv4",
            "byte",
            "k8s-short-name",
        ] {
            assert!(kept("string", f), "{f}");
        }
        assert!(!kept("string", "uint64"));
        assert!(!kept("string", "json"));
        assert!(is_supported_string_format("date-time"));
        assert!(!is_supported_string_format("hostport"));
    }

    #[test]
    fn other_types_lose_any_format() {
        assert!(!kept("boolean", "int32"));
        assert!(!kept("object", "date-time"));
        assert!(!kept("array", "uuid"));
    }

    #[test]
    fn nested_schemas_are_cleaned_and_a_property_named_format_stays() {
        let mut schema = JSONSchemaProps {
            type_: Some("object".to_string()),
            properties: Some(BTreeMap::from([
                ("format".to_string(), props("string", "date-time")),
                ("count".to_string(), props("integer", "uint32")),
                (
                    "list".to_string(),
                    JSONSchemaProps {
                        type_: Some("array".to_string()),
                        items: Some(JSONSchemaPropsOrArray::Schema(Box::new(props(
                            "integer", "uint64",
                        )))),
                        ..JSONSchemaProps::default()
                    },
                ),
                (
                    "map".to_string(),
                    JSONSchemaProps {
                        type_: Some("object".to_string()),
                        additional_properties: Some(JSONSchemaPropsOrBool::Schema(Box::new(
                            props("integer", "uint16"),
                        ))),
                        ..JSONSchemaProps::default()
                    },
                ),
                (
                    "either".to_string(),
                    JSONSchemaProps {
                        any_of: Some(vec![props("integer", "uint8"), props("integer", "int32")]),
                        ..JSONSchemaProps::default()
                    },
                ),
            ])),
            ..JSONSchemaProps::default()
        };
        strip_unsupported_schema_formats(&mut schema);
        let mut found = Vec::new();
        formats(&serde_json::to_value(&schema).unwrap(), "", &mut found);
        found.sort();
        assert_eq!(
            found,
            vec![
                ".properties.either.anyOf[1]: int32 (Some(String(\"integer\")))".to_string(),
                ".properties.format: date-time (Some(String(\"string\")))".to_string(),
            ]
        );
    }

    #[test]
    fn no_shipped_crd_carries_an_unsupported_format() {
        let mut bad = Vec::new();
        for crd in crds() {
            for version in &crd.spec.versions {
                let Some(root) = version
                    .schema
                    .as_ref()
                    .and_then(|s| s.open_api_v3_schema.as_ref())
                else {
                    continue;
                };
                let mut found = Vec::new();
                formats(&serde_json::to_value(root).unwrap(), "", &mut found);
                let integer = |f: &str| f.contains("(Some(String(\"integer\")))");
                let allowed = |f: &str| f.contains(": int32 (") || f.contains(": int64 (");
                for f in found {
                    if f.contains(": uint") || (integer(&f) && !allowed(&f)) {
                        bad.push(format!("{} {}: {f}", crd.spec.names.kind, version.name));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "the apiserver drops these formats:\n{}",
            bad.join("\n")
        );
    }

    #[test]
    fn crds_are_cluster_scoped() {
        for crd in crds() {
            assert_eq!(crd.spec.scope, "Cluster", "{}", crd.spec.names.kind);
        }
    }

    #[test]
    fn storage_mode_schema_keeps_shared_for_api_stability() {
        let yaml = render(&AvailabilityZone::crd()).unwrap();
        assert!(
            yaml.contains("Shared"),
            "Shared must stay in the schema (ADR-0004)"
        );
    }

    #[test]
    fn checked_in_crds_are_current() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/crds");
        for crd in crds() {
            let path = dir.join(file_name(&crd));
            let on_disk = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("{} missing: run `make crds`", path.display()));
            assert_eq!(
                on_disk,
                render(&crd).unwrap(),
                "{} is stale: run `make crds`",
                path.display()
            );
        }
    }

    #[test]
    fn examples_parse_and_validate() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut seen = 0;
        for file in ["00-availabilityzone.yaml", "01-host.yaml"] {
            let text = std::fs::read_to_string(dir.join(file)).unwrap();
            for doc in text.split("\n---\n") {
                let v: serde_json::Value = serde_saphyr::from_str(doc).unwrap();
                match v["kind"].as_str() {
                    Some("AvailabilityZone") => {
                        let z: AvailabilityZone = serde_json::from_value(v).unwrap();
                        z.spec.validate().unwrap();
                        seen += 1;
                    }
                    Some("Host") => {
                        let h: Host = serde_json::from_value(v).unwrap();
                        h.spec.validate().unwrap();
                        seen += 1;
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(seen, 2);
    }
}
