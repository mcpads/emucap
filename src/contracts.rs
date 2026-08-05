use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const CATALOG_ID: &str = "emucap-feature-contracts/v3";
const CONTRACT_VERSION: u32 = 3;
const EXCEPTION_SCHEMA: &str = "emucap-feature-exceptions/v1";

const CATALOG_SOURCE: &str = include_str!("../contracts/catalog.json");
const EXCEPTION_SOURCE: &str = include_str!("../contracts/exceptions.json");

#[derive(Debug, Clone, Deserialize)]
pub struct ContractCatalog {
    pub schema: String,
    pub contract_version: u32,
    pub default_expectations: Vec<String>,
    pub expectations: Vec<Expectation>,
    pub features: Vec<FeatureContract>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Expectation {
    pub id: String,
    pub category: String,
    pub expression: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureContract {
    pub id: String,
    pub surface: String,
    pub methods: Vec<String>,
    #[serde(default)]
    pub route: Option<String>,
    pub temporal_classes: Vec<String>,
    pub disposition: String,
    pub expectations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExceptionRegistry {
    pub schema: String,
    pub contract_catalog: String,
    pub contract_version: u32,
    pub exceptions: Vec<ContractException>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContractException {
    pub id: String,
    pub feature: String,
    pub kind: String,
    pub targets: Vec<String>,
    pub scope: ExceptionScope,
    #[serde(default)]
    pub activation: BTreeMap<String, Value>,
    #[serde(default)]
    pub constraints: BTreeMap<String, Value>,
    #[serde(default)]
    pub authority: BTreeMap<String, String>,
    #[serde(default)]
    pub public_behavior: BTreeMap<String, Value>,
    pub verification: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ExceptionScope {
    pub adapter: String,
    #[serde(default)]
    pub systems: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdvertisedContracts {
    pub catalog: String,
    #[serde(default)]
    pub active_exceptions: Vec<String>,
    #[serde(default)]
    pub constraints: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    pub authority: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ContractAdvertisement {
    #[default]
    Unreported,
    Reported(AdvertisedContracts),
    Malformed(String),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ContractStatus {
    pub catalog: String,
    pub state: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub active_exceptions: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub constraints: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub authority: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

struct Sources {
    catalog: ContractCatalog,
    registry: ExceptionRegistry,
}

fn sources() -> &'static Sources {
    static SOURCES: OnceLock<Sources> = OnceLock::new();
    SOURCES.get_or_init(|| {
        let catalog: ContractCatalog = serde_json::from_str(CATALOG_SOURCE)
            .expect("contracts/catalog.json must be valid JSON");
        let registry: ExceptionRegistry = serde_json::from_str(EXCEPTION_SOURCE)
            .expect("contracts/exceptions.json must be valid JSON");
        let errors = validate_sources(&catalog, &registry);
        assert!(
            errors.is_empty(),
            "invalid contract catalog or exception registry: {errors:?}"
        );
        Sources { catalog, registry }
    })
}

pub fn catalog() -> &'static ContractCatalog {
    &sources().catalog
}

pub fn registry() -> &'static ExceptionRegistry {
    &sources().registry
}

pub fn advertisement_from_hello(hello: &Value) -> ContractAdvertisement {
    let Some(value) = hello.get("contracts") else {
        return ContractAdvertisement::Unreported;
    };
    match serde_json::from_value::<AdvertisedContracts>(value.clone()) {
        Ok(value) => ContractAdvertisement::Reported(value),
        Err(error) => ContractAdvertisement::Malformed(error.to_string()),
    }
}

pub fn advertisement_value(exception_ids: &[&str]) -> Value {
    for id in exception_ids {
        registry()
            .exceptions
            .iter()
            .find(|entry| entry.id == *id)
            .unwrap_or_else(|| panic!("unknown contract exception id: {id}"));
    }
    json!({
        "catalog": CATALOG_ID,
        "active_exceptions": exception_ids,
    })
}

pub fn validate_advertisement(
    advertisement: &ContractAdvertisement,
    adapter: Option<&str>,
    system: Option<&str>,
    methods: &[String],
) -> ContractStatus {
    let mut status = ContractStatus {
        catalog: CATALOG_ID.to_string(),
        state: "unreported".to_string(),
        active_exceptions: Vec::new(),
        constraints: BTreeMap::new(),
        authority: BTreeMap::new(),
        errors: Vec::new(),
    };
    let reported = match advertisement {
        ContractAdvertisement::Unreported => return status,
        ContractAdvertisement::Malformed(error) => {
            status.state = "unvalidated".to_string();
            status
                .errors
                .push(format!("malformed contract advertisement: {error}"));
            return status;
        }
        ContractAdvertisement::Reported(reported) => reported,
    };

    status.catalog = reported.catalog.clone();
    status.active_exceptions = reported.active_exceptions.clone();

    if reported.catalog != CATALOG_ID {
        status.errors.push(format!(
            "unknown contract catalog: {} (expected {CATALOG_ID})",
            reported.catalog
        ));
    }

    let sources = sources();
    let known_methods: BTreeSet<&str> = sources
        .catalog
        .features
        .iter()
        .flat_map(|feature| feature.methods.iter().map(String::as_str))
        .collect();
    for method in methods {
        if !known_methods.contains(method.as_str()) {
            status
                .errors
                .push(format!("method has no feature contract: {method}"));
        }
    }

    let mut seen = BTreeSet::new();
    let mut merged_constraints = BTreeMap::new();
    let mut merged_authority = BTreeMap::new();
    for id in &reported.active_exceptions {
        if !seen.insert(id.as_str()) {
            status
                .errors
                .push(format!("duplicate active exception id: {id}"));
            continue;
        }
        let Some(exception) = sources
            .registry
            .exceptions
            .iter()
            .find(|entry| entry.id == *id)
        else {
            status
                .errors
                .push(format!("unknown active exception id: {id}"));
            continue;
        };
        if adapter != Some(exception.scope.adapter.as_str()) {
            status.errors.push(format!(
                "exception {id} scope adapter={} does not match {:?}",
                exception.scope.adapter, adapter
            ));
        }
        if !exception.scope.systems.is_empty()
            && !system.is_some_and(|value| exception.scope.systems.iter().any(|s| s == value))
        {
            status.errors.push(format!(
                "exception {id} scope systems={:?} does not match {:?}",
                exception.scope.systems, system
            ));
        }
        if let Err(error) = merge_values(
            &mut merged_constraints,
            &exception.constraints,
            "constraint",
            id,
        ) {
            status.errors.push(error);
        }
        if let Err(error) =
            merge_values(&mut merged_authority, &exception.authority, "authority", id)
        {
            status.errors.push(error);
        }
    }
    if let Some(reported_constraints) = &reported.constraints {
        if reported_constraints != &merged_constraints {
            status.errors.push(format!(
                "advertised constraints do not match active exceptions: expected {}",
                serde_json::to_string(&merged_constraints).unwrap_or_default()
            ));
        }
    }
    if let Some(reported_authority) = &reported.authority {
        if reported_authority != &merged_authority {
            status.errors.push(format!(
                "advertised authority does not match active exceptions: expected {}",
                serde_json::to_string(&merged_authority).unwrap_or_default()
            ));
        }
    }
    status.constraints = merged_constraints;
    status.authority = merged_authority;

    status.state = if status.errors.is_empty() {
        "validated"
    } else {
        "unvalidated"
    }
    .to_string();
    status
}

fn merge_values<T>(
    target: &mut BTreeMap<String, T>,
    source: &BTreeMap<String, T>,
    kind: &str,
    exception_id: &str,
) -> Result<(), String>
where
    T: Clone + PartialEq + std::fmt::Debug,
{
    for (key, value) in source {
        if let Some(previous) = target.get(key) {
            if previous != value {
                return Err(format!(
                    "conflicting {kind} {key} while activating {exception_id}: {previous:?} != {value:?}"
                ));
            }
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

pub fn validate_sources(catalog: &ContractCatalog, registry: &ExceptionRegistry) -> Vec<String> {
    let mut errors = Vec::new();
    if catalog.schema != CATALOG_ID {
        errors.push(format!("catalog schema must be {CATALOG_ID}"));
    }
    if catalog.contract_version != CONTRACT_VERSION {
        errors.push(format!(
            "catalog contract_version must be {CONTRACT_VERSION}"
        ));
    }
    if registry.schema != EXCEPTION_SCHEMA {
        errors.push(format!("exception schema must be {EXCEPTION_SCHEMA}"));
    }
    if registry.contract_catalog != catalog.schema {
        errors.push("exception registry references a different catalog".to_string());
    }
    if registry.contract_version != catalog.contract_version {
        errors.push("exception registry contract_version mismatch".to_string());
    }

    let expectation_categories = [
        "precondition",
        "postcondition",
        "invariant",
        "observation",
        "temporal",
    ];
    let mut expectation_ids = BTreeSet::new();
    for expectation in &catalog.expectations {
        if !expectation_ids.insert(expectation.id.as_str()) {
            errors.push(format!("duplicate expectation id: {}", expectation.id));
        }
        if !expectation_categories.contains(&expectation.category.as_str()) {
            errors.push(format!(
                "unknown expectation category {} for {}",
                expectation.category, expectation.id
            ));
        }
        if expectation.expression.trim().is_empty() {
            errors.push(format!("empty expectation expression: {}", expectation.id));
        }
    }
    for id in &catalog.default_expectations {
        if !expectation_ids.contains(id.as_str()) {
            errors.push(format!("unknown default expectation: {id}"));
        }
    }

    let surfaces = ["public", "wire", "test"];
    let routes = ["input_control", "debug", "analysis"];
    let temporal_classes = ["T0", "T0/P", "T1", "T2", "T3", "T4", "T5"];
    let dispositions = ["retain", "consolidate", "migrate", "evaluate_remove"];
    let mut feature_ids = BTreeSet::new();
    let mut method_owners = BTreeMap::new();
    for feature in &catalog.features {
        if !feature_ids.insert(feature.id.as_str()) {
            errors.push(format!("duplicate feature id: {}", feature.id));
        }
        if !surfaces.contains(&feature.surface.as_str()) {
            errors.push(format!(
                "unknown surface {} for {}",
                feature.surface, feature.id
            ));
        }
        if feature.methods.is_empty() {
            errors.push(format!("feature has no methods: {}", feature.id));
        }
        for method in &feature.methods {
            if let Some(previous) = method_owners.insert(method.as_str(), feature.id.as_str()) {
                errors.push(format!(
                    "method {method} belongs to both {previous} and {}",
                    feature.id
                ));
            }
        }
        if feature.surface == "public" {
            match feature.route.as_deref() {
                Some(route) if route.trim().is_empty() => {
                    errors.push(format!("empty public route for {}", feature.id));
                }
                Some(route) if !routes.contains(&route) => {
                    errors.push(format!("unknown public route {route} for {}", feature.id));
                }
                _ => {}
            }
        } else if feature.route.is_some() {
            errors.push(format!(
                "non-public feature {} cannot declare an MCP route",
                feature.id
            ));
        }
        if feature.temporal_classes.is_empty() {
            errors.push(format!("feature has no temporal class: {}", feature.id));
        }
        for class in &feature.temporal_classes {
            if !temporal_classes.contains(&class.as_str()) {
                errors.push(format!("unknown temporal class {class} for {}", feature.id));
            }
        }
        if !dispositions.contains(&feature.disposition.as_str()) {
            errors.push(format!(
                "unknown disposition {} for {}",
                feature.disposition, feature.id
            ));
        }
        for id in &feature.expectations {
            if !expectation_ids.contains(id.as_str()) {
                errors.push(format!("unknown expectation {id} for {}", feature.id));
            }
        }
    }

    let exception_kinds = [
        "capability_absent",
        "precondition_narrowing",
        "parameter_constraint",
        "variation_binding",
        "evidence_constraint",
        "temporal_constraint",
        "fallback_reference",
    ];
    let error_kinds = ["bad_params", "bad_state", "unsupported"];
    let authority_values = ["exact", "current", "best_effort", "unverified", "stale"];
    let mut exception_ids = BTreeSet::new();
    for exception in &registry.exceptions {
        if !exception_ids.insert(exception.id.as_str()) {
            errors.push(format!("duplicate exception id: {}", exception.id));
        }
        if !feature_ids.contains(exception.feature.as_str()) {
            errors.push(format!(
                "exception {} references unknown feature {}",
                exception.id, exception.feature
            ));
        }
        if !exception_kinds.contains(&exception.kind.as_str()) {
            errors.push(format!(
                "exception {} has unknown kind {}",
                exception.id, exception.kind
            ));
        }
        if exception.targets.is_empty() {
            errors.push(format!("exception {} has no targets", exception.id));
        }
        for target in &exception.targets {
            if !expectation_ids.contains(target.as_str()) {
                errors.push(format!(
                    "exception {} references unknown expectation {target}",
                    exception.id
                ));
            }
        }
        if exception.scope.adapter.trim().is_empty() {
            errors.push(format!(
                "exception {} has empty adapter scope",
                exception.id
            ));
        }
        if exception.verification.is_empty() {
            errors.push(format!("exception {} has no verification", exception.id));
        }
        if let Some(invalid) = exception
            .public_behavior
            .get("invalid")
            .and_then(Value::as_str)
        {
            if !error_kinds.contains(&invalid) {
                errors.push(format!(
                    "exception {} has unstable public error kind {invalid}",
                    exception.id
                ));
            }
        }
        for (feature, authority) in &exception.authority {
            if !feature_ids.contains(feature.as_str()) {
                errors.push(format!(
                    "exception {} authority references unknown feature {feature}",
                    exception.id
                ));
            }
            if !authority_values.contains(&authority.as_str()) {
                errors.push(format!(
                    "exception {} has unknown authority {authority}",
                    exception.id
                ));
            }
        }
    }
    errors
}

#[cfg(test)]
#[path = "contracts_tests.rs"]
mod tests;
