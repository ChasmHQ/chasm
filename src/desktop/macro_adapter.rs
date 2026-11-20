use std::collections::HashMap;

use ethers::abi::{param_type::Writer, Abi, Constructor, FunctionExt};
use serde_json::Value;

use super::{deployment::ConstructorSpec, foundry_build::FoundryBuildReport, macro_model as model};
use super::{macro_backend::SignerAddresses, macro_runner as runner};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterError {
    InvalidDefinition,
    IneligibleBuild,
    ArtifactUnavailable,
    InvalidArtifact,
    MissingSigner,
    EmptyPlan,
    AutomaticSnapshotUnsupported,
    AutomaticRollbackUnsupported,
    DynamicCompositeUnsupported,
    DynamicConstructorUnsupported,
    DynamicGasUnsupported,
    OutputUnsupported,
    InvalidReference,
    InvalidArguments,
    RunnerLimit,
}

pub struct AdapterPlan {
    pub plan: runner::ExecutionPlan,
    ids: Vec<model::StepId>,
    #[cfg_attr(not(test), allow(dead_code))]
    numeric_ids: HashMap<model::StepId, runner::StepId>,
    #[cfg_attr(not(test), allow(dead_code))]
    disabled_ids: Vec<model::StepId>,
}

impl AdapterPlan {
    #[cfg(test)]
    pub fn model_id(&self, id: runner::StepId) -> Option<&model::StepId> {
        id.0.checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.ids.get(index))
    }

    #[cfg(test)]
    pub fn runner_id(&self, id: &model::StepId) -> Option<runner::StepId> {
        self.numeric_ids.get(id).copied()
    }

    #[cfg(test)]
    pub fn disabled_ids(&self) -> &[model::StepId] {
        &self.disabled_ids
    }

    pub fn plan_ids(&self) -> Vec<model::StepId> {
        self.ids.clone()
    }
}

struct Mapper<'a> {
    definition: &'a model::MacroDefinition,
    artifacts: &'a [super::foundry_build::FoundryArtifactSummary],
    addresses: &'a SignerAddresses,
    ids: HashMap<model::StepId, runner::StepId>,
    previous: HashMap<model::StepId, &'a model::StepKind>,
}

impl Mapper<'_> {
    fn artifact(
        &self,
        id: &str,
    ) -> Result<&super::foundry_build::FoundryArtifactSummary, AdapterError> {
        let mut matches = self.artifacts.iter().filter(|item| item.id == id);
        let item = matches.next().ok_or(AdapterError::ArtifactUnavailable)?;
        if matches.next().is_some() || !item.deployable() {
            return Err(AdapterError::ArtifactUnavailable);
        }
        Ok(item)
    }

    fn alias(&self, signer: &model::Signer) -> Result<runner::SignerAlias, AdapterError> {
        let alias = match signer {
            model::Signer::AnvilAccount0 => runner::SignerAlias::Handle(0),
            model::Signer::AnvilAccount1 => runner::SignerAlias::Handle(1),
            model::Signer::AnvilAccount2 => runner::SignerAlias::Handle(2),
            model::Signer::UnlockedWalletRef => runner::SignerAlias::Active,
        };
        self.addresses
            .address(alias)
            .ok_or(AdapterError::MissingSigner)?;
        Ok(alias)
    }

    fn caller(&self, signer: &model::Signer) -> Result<runner::Input, AdapterError> {
        let alias = self.alias(signer)?;
        Ok(runner::Input::Literal(Value::String(
            self.addresses
                .address(alias)
                .ok_or(AdapterError::MissingSigner)?
                .to_owned(),
        )))
    }

    fn input(&self, value: &Value) -> Result<runner::Input, AdapterError> {
        if let Some(text) = value.as_str() {
            if text.contains("${") || text.contains('}') {
                return match model::parse_reference(text)
                    .map_err(|_| AdapterError::InvalidReference)?
                {
                    model::Reference::Step { id, field } => {
                        let step = *self.ids.get(&id).ok_or(AdapterError::InvalidReference)?;
                        let kind = self
                            .previous
                            .get(&id)
                            .ok_or(AdapterError::InvalidReference)?;
                        let pointer = match (field.as_str(), *kind) {
                            ("contractAddress", model::StepKind::DeployArtifact { .. }) => {
                                "/address"
                            }
                            ("txHash", _) => "/transaction_hash",
                            ("blockNumber", _) => "/block_number",
                            ("gasUsed", _) => "/gas_used",
                            ("returnData", model::StepKind::RawCall { .. })
                            | (
                                "returnData",
                                model::StepKind::AbiCall {
                                    mode: model::AbiCallMode::Read,
                                    ..
                                },
                            ) => "/value",
                            ("selectedGas", model::StepKind::GasSweep { .. } | model::StepKind::ProbeGasSweep { .. }) => "/gas",
                            ("storageValue", model::StepKind::StorageRead { .. }) => "/value",
                            ("passed", model::StepKind::Assert { .. }) => "/passed",
                            _ => return Err(AdapterError::OutputUnsupported),
                        };
                        Ok(runner::Input::Previous {
                            step,
                            pointer: pointer.into(),
                        })
                    }
                    model::Reference::Account { index } => {
                        let address = self
                            .addresses
                            .address(runner::SignerAlias::Handle(u64::from(index)))
                            .ok_or(AdapterError::MissingSigner)?;
                        Ok(runner::Input::Literal(Value::String(address.to_owned())))
                    }
                    model::Reference::Variable { id } => {
                        let value = self
                            .definition
                            .variables
                            .get(&id)
                            .ok_or(AdapterError::InvalidReference)?;
                        Ok(runner::Input::Literal(value.clone()))
                    }
                };
            }
        }
        Ok(runner::Input::Literal(self.materialize(value)?))
    }

    fn materialize(&self, value: &Value) -> Result<Value, AdapterError> {
        match value {
            Value::String(text) if text.contains("${") || text.contains('}') => match self
                .input(value)?
            {
                runner::Input::Literal(value) => Ok(value),
                runner::Input::Previous { .. } => Err(AdapterError::DynamicCompositeUnsupported),
            },
            Value::Array(items) => items.iter().map(|item| self.materialize(item)).collect(),
            Value::Object(items) => items
                .iter()
                .map(|(key, value)| Ok((key.clone(), self.materialize(value)?)))
                .collect(),
            _ => Ok(value.clone()),
        }
    }

    fn field(&self, text: &str) -> Result<runner::Input, AdapterError> {
        self.input(&Value::String(text.to_owned()))
    }

    fn probes(
        &self,
        probes: &[model::StorageProbe],
    ) -> Result<Vec<runner::StorageProbe>, AdapterError> {
        probes
            .iter()
            .map(|probe| {
                Ok(runner::StorageProbe {
                    address: self.field(&probe.address)?,
                    slot: self.field(&probe.slot)?,
                })
            })
            .collect()
    }

    fn transaction(
        &self,
        target: &str,
        data: &str,
        value: &str,
        signer: &model::Signer,
    ) -> Result<runner::RawTransactionInput, AdapterError> {
        Ok(runner::RawTransactionInput {
            to: self.field(target)?,
            data: self.field(data)?,
            value: self.field(value)?,
            from: Some(self.caller(signer)?),
        })
    }

    fn arguments(&self, args: &[Value]) -> Result<Value, AdapterError> {
        Ok(Value::Array(
            args.iter()
                .map(|value| self.materialize(value))
                .collect::<Result<_, _>>()?,
        ))
    }

    fn gas(&self, text: &str) -> Result<runner::GasLimit, AdapterError> {
        match self.field(text)? {
            runner::Input::Literal(value) => {
                let gas = value
                    .as_str()
                    .and_then(|text| model::parse_gas(text).ok())
                    .filter(|gas| *gas > 0)
                    .ok_or(AdapterError::InvalidArguments)?;
                Ok(runner::GasLimit::Exact(gas))
            }
            runner::Input::Previous { step, pointer } if pointer == "/gas" => {
                Ok(runner::GasLimit::FromSweep(step))
            }
            runner::Input::Previous { .. } => Err(AdapterError::DynamicGasUnsupported),
        }
    }

    fn operation(&self, kind: &model::StepKind) -> Result<runner::Operation, AdapterError> {
        use model::StepKind as M;
        use runner::Operation as R;
        Ok(match kind {
            M::DeployArtifact {
                artifact_id,
                constructor_signature,
                args,
                signer,
                value,
                gas_limit,
            } => {
                let artifact = self.artifact(artifact_id)?;
                let spec = ConstructorSpec::from_abi(&artifact.abi)
                    .map_err(|_| AdapterError::InvalidArtifact)?;
                let params = spec
                    .as_ref()
                    .map(|spec| spec.inputs.as_slice())
                    .unwrap_or(&[]);
                let signature = format!(
                    "constructor({})",
                    params
                        .iter()
                        .map(|param| Writer::write(&param.kind))
                        .collect::<Vec<_>>()
                        .join(",")
                );
                if &signature != constructor_signature || params.len() != args.len() {
                    return Err(AdapterError::InvalidArguments);
                }
                let values = self.arguments(args).map_err(|error| match error {
                    AdapterError::DynamicCompositeUnsupported => {
                        AdapterError::DynamicConstructorUnsupported
                    }
                    other => other,
                })?;
                let mut remaining = 4096;
                let tokens = params
                    .iter()
                    .zip(values.as_array().ok_or(AdapterError::InvalidArguments)?)
                    .map(|(param, value)| {
                        let raw = match (&param.kind, value) {
                            (ethers::abi::ParamType::Bool, Value::Bool(value)) => value.to_string(),
                            (
                                ethers::abi::ParamType::Array(_)
                                | ethers::abi::ParamType::FixedArray(_, _)
                                | ethers::abi::ParamType::Tuple(_),
                                Value::Array(_),
                            ) => value.to_string(),
                            (_, Value::String(text)) => text.clone(),
                            _ => return Err(AdapterError::InvalidArguments),
                        };
                        super::deployment::parse_field(&param.kind, &raw, &mut remaining)
                            .map_err(|_| AdapterError::InvalidArguments)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let bytecode = artifact
                    .creation_bytecode
                    .as_deref()
                    .ok_or(AdapterError::ArtifactUnavailable)?;
                let digits = bytecode.strip_prefix("0x").unwrap_or(bytecode);
                if digits.is_empty()
                    || digits.len() % 2 != 0
                    || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(AdapterError::InvalidArtifact);
                }
                let code = ethers::utils::hex::decode(digits)
                    .map_err(|_| AdapterError::InvalidArtifact)?;
                let creation = Constructor {
                    inputs: params.to_vec(),
                }
                .encode_input(code, &tokens)
                .map_err(|_| AdapterError::InvalidArguments)?;
                let creation = format!("0x{}", ethers::utils::hex::encode(creation));
                if creation.len() > 8 * 1024 * 1024 {
                    return Err(AdapterError::RunnerLimit);
                }
                if !spec.as_ref().is_some_and(|spec| spec.payable) {
                    match self.field(value)? {
                        runner::Input::Literal(Value::String(text))
                            if model::parse_quantity(&text).is_ok_and(|value| value.is_zero()) => {}
                        _ => return Err(AdapterError::InvalidArguments),
                    }
                }
                R::Deploy {
                    creation: runner::Input::Literal(Value::String(creation)),
                    value: self.field(value)?,
                    gas: self.field(gas_limit)?,
                    signer: self.alias(signer)?,
                }
            }
            M::AbiCall {
                artifact_id,
                signature,
                mode,
                target,
                args,
                signer,
                value,
                gas_limit,
            } => {
                let artifact = self.artifact(artifact_id)?;
                let abi: Abi = serde_json::from_value(artifact.abi.clone())
                    .map_err(|_| AdapterError::InvalidArtifact)?;
                let mut functions = abi
                    .functions()
                    .filter(|function| function.abi_signature() == *signature);
                let function = functions.next().ok_or(AdapterError::InvalidArguments)?;
                if functions.next().is_some()
                    || function.inputs.len() != args.len()
                    || signature.len() > 512
                {
                    return Err(AdapterError::InvalidArguments);
                }
                let arguments = self.arguments(args)?;
                R::AbiCall(runner::AbiInput {
                    address: self.field(target)?,
                    abi: runner::Input::Literal(artifact.abi.clone()),
                    signature: signature.clone(),
                    arguments: runner::Input::Literal(arguments),
                    value: self.field(value)?,
                    from: Some(self.caller(signer)?),
                    gas: Some(self.field(gas_limit)?),
                    signer: match mode {
                        model::AbiCallMode::Read => None,
                        model::AbiCallMode::Send => Some(self.alias(signer)?),
                    },
                })
            }
            M::RawCall {
                target,
                data,
                signer,
                value,
                gas_limit,
            } => R::RawCall {
                transaction: self.transaction(target, data, value, signer)?,
                gas: Some(self.field(gas_limit)?),
            },
            M::RawSend {
                target,
                data,
                signer,
                value,
                gas_limit,
            } => {
                let gas = self.gas(gas_limit)?;
                if let runner::GasLimit::FromSweep(id) = &gas {
                    let matches_sweep = self.previous.iter().any(|(previous_id, kind)| {
                        if self.ids.get(previous_id) != Some(id) {
                            return false;
                        }
                        match kind {
                            model::StepKind::GasSweep { target: prior_target, data: prior_data, signer: prior_signer, value: prior_value, .. } => {
                                prior_target == target && prior_data == data && prior_signer == signer && prior_value == value
                            }
                            model::StepKind::ProbeGasSweep { router, data: prior_data, signer: prior_signer, value: prior_value, .. } => {
                                router == target && prior_data == data && prior_signer == signer && prior_value == value && value == "0"
                            }
                            _ => false,
                        }
                    });
                    if !matches_sweep {
                        return Err(AdapterError::InvalidArguments);
                    }
                }
                R::RawSend {
                    transaction: self.transaction(target, data, value, signer)?,
                    gas,
                    signer: self.alias(signer)?,
                }
            }
            M::StorageRead { target, slot } => R::Storage(runner::StorageProbe {
                address: self.field(target)?,
                slot: self.field(slot)?,
            }),
            M::Snapshot { probes } => R::Snapshot {
                probes: self.probes(probes)?,
            },
            M::Revert { snapshot } => {
                let model::Reference::Step { id, field } =
                    model::parse_reference(snapshot).map_err(|_| AdapterError::InvalidReference)?
                else {
                    return Err(AdapterError::InvalidReference);
                };
                if field != "snapshotId" {
                    return Err(AdapterError::InvalidReference);
                }
                R::Revert {
                    snapshot: *self.ids.get(&id).ok_or(AdapterError::InvalidReference)?,
                }
            }
            M::ProbeGasSweep {
                artifact_id, probe, router, data, token, treasury, signer, value,
                start, end, increment, max_attempts, outer_gas_margin, final_gas_margin,
            } => {
                let artifact = self.artifact(artifact_id)?;
                let abi: Abi = serde_json::from_value(artifact.abi.clone())
                    .map_err(|_| AdapterError::InvalidArtifact)?;
                if artifact.contract != "GasProbe" || !abi.functions().any(|function| function.abi_signature() == "probe(address,uint256,bytes,address,address)") {
                    return Err(AdapterError::InvalidArtifact);
                }
                if value != "0" || *max_attempts == 0 {
                    return Err(AdapterError::InvalidArguments);
                }
                R::ProbeGasSweep {
                    probe: self.field(probe)?, router: self.field(router)?,
                    data: self.field(data)?, token: self.field(token)?,
                    treasury: self.field(treasury)?, from: self.caller(signer)?,
                    start: model::parse_gas(start).map_err(|_| AdapterError::InvalidArguments)?,
                    end: model::parse_gas(end).map_err(|_| AdapterError::InvalidArguments)?,
                    increment: model::parse_gas(increment).map_err(|_| AdapterError::InvalidArguments)?,
                    max_attempts: *max_attempts,
                    outer_gas_margin: model::parse_gas(outer_gas_margin).map_err(|_| AdapterError::InvalidArguments)?,
                    final_gas_margin: model::parse_gas(final_gas_margin).map_err(|_| AdapterError::InvalidArguments)?,
                }
            }
            M::GasSweep {
                target,
                data,
                signer,
                value,
                start,
                end,
                increment,
                max_attempts,
                predicate,
            } => {
                let predicate = match predicate.as_ref().ok_or(AdapterError::InvalidArguments)? {
                    model::SweepPredicate::Equals { value } => {
                        runner::SweepPredicate::Equals(Value::String(value.clone()))
                    }
                    model::SweepPredicate::Zero {} => runner::SweepPredicate::Zero,
                    model::SweepPredicate::Nonzero {} => runner::SweepPredicate::NonZero,
                    model::SweepPredicate::Revert {} => runner::SweepPredicate::Revert,
                    model::SweepPredicate::Success {} => runner::SweepPredicate::Success,
                };
                R::GasSweep {
                    transaction: self.transaction(target, data, value, signer)?,
                    start: model::parse_gas(start).map_err(|_| AdapterError::InvalidArguments)?,
                    end: model::parse_gas(end).map_err(|_| AdapterError::InvalidArguments)?,
                    increment: model::parse_gas(increment)
                        .map_err(|_| AdapterError::InvalidArguments)?,
                    max_attempts: *max_attempts,
                    predicate,
                }
            }
            M::Assert {
                left,
                comparator,
                right,
            } => {
                let comparator = match comparator {
                    model::AssertComparator::Eq => runner::AssertComparator::Eq,
                    model::AssertComparator::NotEq => runner::AssertComparator::NotEq,
                    model::AssertComparator::Zero => runner::AssertComparator::Zero,
                    model::AssertComparator::Nonzero => runner::AssertComparator::NonZero,
                    model::AssertComparator::True => runner::AssertComparator::True,
                    model::AssertComparator::False => runner::AssertComparator::False,
                    model::AssertComparator::HashEq => runner::AssertComparator::HashEq,
                };
                let left = self.field(left)?;
                if matches!(&left, runner::Input::Previous { pointer, .. } if pointer == "/gas")
                    && matches!(
                        comparator,
                        runner::AssertComparator::Eq
                            | runner::AssertComparator::NotEq
                            | runner::AssertComparator::HashEq
                    )
                {
                    return Err(AdapterError::OutputUnsupported);
                }
                let right = right.as_ref().map(|right| self.input(right)).transpose()?;
                if matches!(&right, Some(runner::Input::Previous { pointer, .. }) if pointer == "/gas")
                {
                    return Err(AdapterError::OutputUnsupported);
                }
                R::Assert {
                    left,
                    comparator,
                    right,
                }
            }
            M::Delay { millis } => R::Delay {
                milliseconds: *millis,
            },
        })
    }
}

pub fn adapt(
    definition: &model::MacroDefinition,
    report: &FoundryBuildReport,
    addresses: &SignerAddresses,
    token: runner::ExecutionToken,
    dry_run: bool,
) -> Result<AdapterPlan, AdapterError> {
    adapt_checked(
        definition,
        report.exact_target_eligible(),
        &report.artifacts,
        addresses,
        token,
        dry_run,
    )
}

/// Plans whose steps never resolve a compiled artifact (raw calls and sends,
/// storage reads, snapshots, asserts, delays) need no Trusted Foundry Build.
pub fn adapt_without_artifacts(
    definition: &model::MacroDefinition,
    addresses: &SignerAddresses,
    token: runner::ExecutionToken,
    dry_run: bool,
) -> Result<AdapterPlan, AdapterError> {
    adapt_checked(definition, true, &[], addresses, token, dry_run)
}

pub fn needs_artifacts(definition: &model::MacroDefinition) -> bool {
    definition.steps.iter().any(|step| {
        step.enabled
            && matches!(
                step.kind,
                model::StepKind::DeployArtifact { .. }
                    | model::StepKind::AbiCall { .. }
                    | model::StepKind::ProbeGasSweep { .. }
            )
    })
}

fn adapt_checked(
    definition: &model::MacroDefinition,
    eligible: bool,
    artifacts: &[super::foundry_build::FoundryArtifactSummary],
    addresses: &SignerAddresses,
    token: runner::ExecutionToken,
    dry_run: bool,
) -> Result<AdapterPlan, AdapterError> {
    definition
        .validate()
        .map_err(|_| AdapterError::InvalidDefinition)?;
    if !eligible {
        return Err(AdapterError::IneligibleBuild);
    }
    if definition.revert_on_failure {
        return Err(AdapterError::AutomaticRollbackUnsupported);
    }
    if definition.snapshot_before_run {
        return Err(AdapterError::AutomaticSnapshotUnsupported);
    }
    let mut mapper = Mapper {
        definition,
        artifacts,
        addresses,
        ids: HashMap::new(),
        previous: HashMap::new(),
    };
    let mut ids = Vec::new();
    let mut disabled_ids = Vec::new();
    let mut steps = Vec::new();
    for step in &definition.steps {
        if !step.enabled {
            disabled_ids.push(step.id.clone());
            continue;
        }
        let id = runner::StepId(steps.len() as u64 + 1);
        let operation = mapper.operation(&step.kind)?;
        ids.push(step.id.clone());
        mapper.ids.insert(step.id.clone(), id);
        mapper.previous.insert(step.id.clone(), &step.kind);
        steps.push(runner::PlanStep { id, operation });
    }
    if steps.is_empty() {
        return Err(AdapterError::EmptyPlan);
    }
    Ok(AdapterPlan {
        plan: runner::ExecutionPlan {
            token,
            dry_run,
            steps,
            bundle: definition.bundle,
        },
        ids,
        numeric_ids: mapper.ids,
        disabled_ids,
    })
}

#[cfg(test)]
#[path = "macro_adapter_tests.rs"]
mod tests;
