//! Common request normalization and reply verification for `execution_speed`.
//!
//! Percent is a host pacing target for guest-time progress (100 = normal). Wire numbers are
//! normalized to hundredths of a percent; a value that is not exactly representable there, or that
//! lies outside the adapter's advertised domain, is rejected instead of being clamped or rounded.
//! The producer owns applying, reading back and restoring the native policy.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const METHOD: &str = "execution_speed";
pub const SCOPE_HOST_PACING: &str = "host_pacing";
/// Every adapter that advertises pacing admits at least these targets and `unlimited`.
pub const REQUIRED_PERCENTS: [u64; 4] = [50, 100, 200, 400];
const CENTI: u64 = 100;
const MAX_CENTI: u64 = 100_000 * CENTI;
/// Upper bound on the advertised host-side service gap while a pacing wait is in progress.
pub const MAX_CONTROL_SERVICE_MS: u64 = 1_000;

/// Static pacing domain advertised in hello. Part of the capability revision; the current policy
/// is dynamic and carries its own `policy_revision` instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSpeedCapability {
    pub modes: Vec<String>,
    pub percent: PercentDomain,
    /// Execution states in which the policy can be queried and changed.
    pub states: Vec<String>,
    pub scope: String,
    /// `native` for an emulator-owned governor, `frontend` for an adapter-owned run loop.
    pub source: String,
    /// Longest host gap between control-service opportunities during a pacing wait.
    pub control_service_ms: u64,
    /// Host facilities that may hold throughput below the target, such as VSync or audio queues.
    #[serde(default)]
    pub host_constraints: Vec<String>,
}

/// Either a stepped range or an explicit value list, both in percent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PercentDomain {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<f64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedRequest {
    Query,
    /// Target in hundredths of a percent.
    Limited {
        centi_percent: u64,
    },
    Unlimited,
}

/// Convert a wire percent to hundredths of a percent. `None` when it is not finite, not positive,
/// too large, or not exactly representable at that precision.
pub fn centi_percent(percent: f64) -> Option<u64> {
    if !percent.is_finite() || percent <= 0.0 {
        return None;
    }
    let scaled = percent * CENTI as f64;
    let rounded = scaled.round();
    // Absorb binary floating-point representation error only; never round a real fraction.
    if (scaled - rounded).abs() > 4.0 * f64::EPSILON * scaled.max(1.0) || rounded < 1.0 {
        return None;
    }
    let centi = rounded as u64;
    (centi <= MAX_CENTI).then_some(centi)
}

pub fn percent_value(centi_percent: u64) -> Value {
    if centi_percent.is_multiple_of(CENTI) {
        json!(centi_percent / CENTI)
    } else {
        json!(centi_percent as f64 / CENTI as f64)
    }
}

pub fn parse_request(mode: Option<&str>, percent: Option<f64>) -> Result<SpeedRequest, String> {
    match (mode, percent) {
        (None, None) => Ok(SpeedRequest::Query),
        (Some("unlimited"), None) => Ok(SpeedRequest::Unlimited),
        (Some("unlimited"), Some(_)) => Err("unlimited does not take percent".into()),
        (Some("limited"), Some(percent)) => centi_percent(percent)
            .map(|centi_percent| SpeedRequest::Limited { centi_percent })
            .ok_or_else(|| {
                format!("percent {percent} is not a positive value with at most two decimal places")
            }),
        (Some("limited"), None) => Err("limited requires percent".into()),
        (None, Some(_)) => Err("percent requires mode=limited".into()),
        (Some(other), _) => Err(format!("unknown execution speed mode {other}")),
    }
}

pub fn request_params(request: SpeedRequest) -> Value {
    match request {
        SpeedRequest::Query => json!({}),
        SpeedRequest::Unlimited => json!({"mode": "unlimited"}),
        SpeedRequest::Limited { centi_percent } => {
            json!({"mode": "limited", "percent": percent_value(centi_percent)})
        }
    }
}

impl PercentDomain {
    fn validate(&self) -> Result<(), String> {
        match (self.min, self.max, self.quantum, &self.values) {
            (Some(min), Some(max), Some(quantum), None) => {
                let (min, max, quantum) = (
                    centi_percent(min).ok_or("percent.min is not representable")?,
                    centi_percent(max).ok_or("percent.max is not representable")?,
                    centi_percent(quantum).ok_or("percent.quantum is not representable")?,
                );
                if min > max {
                    return Err("percent.min exceeds percent.max".into());
                }
                if !(max - min).is_multiple_of(quantum) {
                    return Err("percent.max is not reachable from min by quantum".into());
                }
                Ok(())
            }
            (None, None, None, Some(values)) => {
                if values.is_empty() {
                    return Err("percent.values must not be empty".into());
                }
                let mut centi = Vec::with_capacity(values.len());
                for value in values {
                    centi.push(
                        centi_percent(*value).ok_or("percent.values entry is not representable")?,
                    );
                }
                if centi.windows(2).any(|pair| pair[0] >= pair[1]) {
                    return Err("percent.values must be strictly increasing".into());
                }
                Ok(())
            }
            _ => Err("percent must be either {min,max,quantum} or {values}".into()),
        }
    }

    pub fn contains(&self, centi: u64) -> bool {
        if let Some(values) = &self.values {
            return values
                .iter()
                .any(|value| centi_percent(*value) == Some(centi));
        }
        let (Some(min), Some(max), Some(quantum)) = (
            self.min.and_then(centi_percent),
            self.max.and_then(centi_percent),
            self.quantum.and_then(centi_percent),
        ) else {
            return false;
        };
        (min..=max).contains(&centi) && (centi - min).is_multiple_of(quantum)
    }
}

impl ExecutionSpeedCapability {
    pub fn from_hello(value: &Value) -> Result<Self, String> {
        let capability: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid execution_speed_capability: {error}"))?;
        capability.validate()?;
        Ok(capability)
    }

    fn validate(&self) -> Result<(), String> {
        let has = |values: &[String], name: &str| values.iter().any(|value| value == name);
        if !has(&self.modes, "limited") || !has(&self.modes, "unlimited") || self.modes.len() != 2 {
            return Err("modes must be exactly limited and unlimited".into());
        }
        if !has(&self.states, "running") || !has(&self.states, "frozen") {
            return Err("pacing must be changeable while running and frozen".into());
        }
        if self.scope != SCOPE_HOST_PACING {
            return Err(format!("scope must be {SCOPE_HOST_PACING}"));
        }
        if !matches!(self.source.as_str(), "native" | "frontend") {
            return Err("source must be native or frontend".into());
        }
        if !(1..=MAX_CONTROL_SERVICE_MS).contains(&self.control_service_ms) {
            return Err(format!(
                "control_service_ms must be 1..={MAX_CONTROL_SERVICE_MS}"
            ));
        }
        if self.host_constraints.iter().any(String::is_empty) {
            return Err("host_constraints entries must be non-empty".into());
        }
        self.percent.validate()?;
        for required in REQUIRED_PERCENTS {
            if !self.percent.contains(required * CENTI) {
                return Err(format!("percent domain must admit {required}"));
            }
        }
        Ok(())
    }

    pub fn admit(&self, request: SpeedRequest) -> Result<(), String> {
        match request {
            SpeedRequest::Limited { centi_percent } if !self.percent.contains(centi_percent) => {
                Err(format!(
                    "percent {} is outside the advertised domain {}",
                    percent_value(centi_percent),
                    json!(self.percent)
                ))
            }
            _ => Ok(()),
        }
    }

    /// Verify one effective-policy object (query reply, `status.execution_speed`, or the
    /// `previous`/`execution_speed` members of a change reply).
    pub fn verify_policy(&self, policy: &Value) -> Result<(), String> {
        let mode = policy.get("mode").and_then(Value::as_str);
        let percent = policy.get("percent").ok_or("policy.percent is missing")?;
        match mode {
            Some("limited") => {
                let centi = percent
                    .as_f64()
                    .and_then(centi_percent)
                    .ok_or("limited policy requires a representable percent")?;
                if !self.percent.contains(centi) {
                    return Err(
                        "limited policy percent is outside the domain; report custom".into(),
                    );
                }
            }
            Some("unlimited" | "custom") if percent.is_null() => {}
            Some("unlimited" | "custom") => {
                return Err("unlimited and custom policies report percent=null".into())
            }
            _ => return Err("policy.mode must be limited, unlimited or custom".into()),
        }
        if policy.get("source").and_then(Value::as_str) != Some(self.source.as_str()) {
            return Err("policy.source differs from the capability".into());
        }
        let revision = policy.get("policy_revision").and_then(Value::as_str);
        if revision.is_none_or(str::is_empty) {
            return Err("policy.policy_revision is missing".into());
        }
        let constraints = policy
            .get("host_constraints")
            .and_then(Value::as_array)
            .ok_or("policy.host_constraints is missing")?;
        if constraints.iter().any(|value| !value.is_string()) {
            return Err("policy.host_constraints entries must be strings".into());
        }
        Ok(())
    }

    /// Verify a change reply against its admitted request. The confirmed policy must be the
    /// requested one; a successful limited request is never accepted as custom.
    pub fn verify_change(&self, request: SpeedRequest, reply: &Value) -> Result<(), String> {
        if reply.get("status").and_then(Value::as_str) != Some("completed") {
            return Err("change reply status is not completed".into());
        }
        if !matches!(
            reply.get("state").and_then(Value::as_str),
            Some("frozen" | "running")
        ) {
            return Err("change reply state must be frozen or running".into());
        }
        let previous = reply.get("previous").ok_or("change reply lacks previous")?;
        self.verify_policy(previous)?;
        let policy = reply
            .get("execution_speed")
            .ok_or("change reply lacks execution_speed")?;
        self.verify_policy(policy)?;
        let confirmed = match request {
            SpeedRequest::Unlimited => policy["mode"] == "unlimited",
            SpeedRequest::Limited { centi_percent } => {
                policy["mode"] == "limited"
                    && policy["percent"].as_f64().and_then(self::centi_percent)
                        == Some(centi_percent)
            }
            SpeedRequest::Query => false,
        };
        if !confirmed {
            return Err("confirmed policy differs from the request".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "pacing_tests.rs"]
mod tests;
