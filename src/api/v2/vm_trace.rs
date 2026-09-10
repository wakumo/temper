use serde_json::{json, Value};
use std::collections::HashMap;

const MAX_RECURSION: usize = 128;
const MAX_STEPS: usize = 1_000_000;
const MAX_MEMORY: usize = 32 * 1024 * 1024;
const MAX_LOGS: usize = 10_000;
const MAX_STACK: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Word([u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallMode {
    Call,
    CallCode,
    DelegateCall,
    StaticCall,
    Create,
    Create2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpKind {
    Normal,
    Dup(usize),
    Swap(usize),
    Log(usize),
    Call(CallMode),
    SelfDestruct,
}

#[derive(Clone, Copy, Debug)]
struct OpSpec {
    pops: usize,
    pushes: usize,
    kind: OpKind,
}

#[derive(Clone, Debug)]
enum TraceKind {
    Call {
        call_type: String,
        to: String,
    },
    Create {
        address: Option<String>,
    },
    Suicide {
        address: String,
        refund_address: String,
    },
    Other(String),
}

#[derive(Clone, Debug)]
struct Frame {
    failed: bool,
    kind: TraceKind,
}

struct TraceTree {
    frames: HashMap<Vec<usize>, Frame>,
    children: HashMap<Vec<usize>, Vec<Vec<usize>>>,
}

#[derive(Default)]
struct Budget {
    steps: usize,
    logs: usize,
}

/// Reconstructs committed EVM logs from an Erigon/Parity `vmTrace` and its
/// corresponding flat Parity `trace` frames.
pub fn extract_logs(vm_trace: &Value, traces: &[Value]) -> Result<Vec<Value>, String> {
    if root_failed(traces)? {
        return Ok(Vec::new());
    }

    let tree = TraceTree::parse(traces)?;
    let root_path = Vec::new();
    let root = tree
        .frames
        .get(&root_path)
        .ok_or_else(|| "missing root trace frame".to_string())?;
    let context = match &root.kind {
        TraceKind::Call { to, .. } => Some(to.clone()),
        TraceKind::Create { address } => address.clone(),
        TraceKind::Suicide { .. } => {
            return Err("root trace frame cannot be a suicide".to_string());
        }
        TraceKind::Other(kind) => {
            return Err(format!("unsupported root trace type {kind}"));
        }
    };
    if context.is_none() {
        return Err("successful root create trace is missing result.address".to_string());
    }

    let mut budget = Budget::default();
    process_frame(vm_trace, &root_path, context, &tree, &mut budget, 1)
}

fn root_failed(traces: &[Value]) -> Result<bool, String> {
    let mut roots = traces.iter().filter(|trace| {
        trace
            .get("traceAddress")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    });
    let root = roots
        .next()
        .ok_or_else(|| "missing root trace frame".to_string())?;
    if roots.next().is_some() {
        return Err("duplicate root trace frame".to_string());
    }
    Ok(root.get("error").is_some_and(|error| !error.is_null()))
}

impl TraceTree {
    fn parse(traces: &[Value]) -> Result<Self, String> {
        if traces.len() > MAX_STEPS {
            return Err(format!("too many trace frames: maximum is {MAX_STEPS}"));
        }

        let mut frames = HashMap::new();
        for trace in traces {
            let path = parse_trace_address(trace)?;
            let failed = trace.get("error").is_some_and(|error| !error.is_null());
            let kind_name = trace
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("trace frame {path:?} is missing type"))?
                .to_ascii_lowercase();
            let kind = match kind_name.as_str() {
                "call" => {
                    let action = trace
                        .get("action")
                        .and_then(Value::as_object)
                        .ok_or_else(|| format!("call trace frame {path:?} is missing action"))?;
                    let call_type = action
                        .get("callType")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            format!("call trace frame {path:?} is missing action.callType")
                        })?
                        .to_ascii_lowercase();
                    let to =
                        normalize_address(action.get("to").and_then(Value::as_str).ok_or_else(
                            || format!("call trace frame {path:?} is missing action.to"),
                        )?)?;
                    TraceKind::Call { call_type, to }
                }
                "create" => {
                    let address = trace
                        .get("result")
                        .and_then(|result| result.get("address"))
                        .and_then(Value::as_str)
                        .map(normalize_address)
                        .transpose()?;
                    if !failed && address.is_none() {
                        return Err(format!(
                            "successful create trace frame {path:?} is missing result.address"
                        ));
                    }
                    TraceKind::Create { address }
                }
                "suicide" => {
                    let action = trace
                        .get("action")
                        .and_then(Value::as_object)
                        .ok_or_else(|| format!("suicide trace frame {path:?} is missing action"))?;
                    let address = normalize_address(
                        action
                            .get("address")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                format!("suicide trace frame {path:?} is missing action.address")
                            })?,
                    )?;
                    let refund_address = normalize_address(
                        action
                            .get("refundAddress")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                format!(
                                    "suicide trace frame {path:?} is missing action.refundAddress"
                                )
                            })?,
                    )?;
                    TraceKind::Suicide {
                        address,
                        refund_address,
                    }
                }
                other => TraceKind::Other(other.to_string()),
            };
            if frames
                .insert(path.clone(), Frame { failed, kind })
                .is_some()
            {
                return Err(format!("duplicate trace frame {path:?}"));
            }
        }

        if !frames.contains_key(&Vec::new()) {
            return Err("missing root trace frame".to_string());
        }

        let mut children: HashMap<Vec<usize>, Vec<Vec<usize>>> = HashMap::new();
        for path in frames.keys() {
            if path.is_empty() {
                continue;
            }
            let parent = path[..path.len() - 1].to_vec();
            if !frames.contains_key(&parent) {
                return Err(format!("trace frame {path:?} has no parent frame"));
            }
            children.entry(parent).or_default().push(path.clone());
        }
        for paths in children.values_mut() {
            paths.sort_by_key(|path| path[path.len() - 1]);
            for (expected, path) in paths.iter().enumerate() {
                if path[path.len() - 1] != expected {
                    return Err(format!(
                        "trace child addresses are not contiguous at parent {:?}",
                        &path[..path.len() - 1]
                    ));
                }
            }
        }

        Ok(Self { frames, children })
    }
}

fn process_frame(
    vm_trace: &Value,
    path: &[usize],
    context: Option<String>,
    tree: &TraceTree,
    budget: &mut Budget,
    depth: usize,
) -> Result<Vec<Value>, String> {
    if depth > MAX_RECURSION {
        return Err(format!(
            "vmTrace recursion exceeds maximum depth {MAX_RECURSION}"
        ));
    }
    let frame = tree
        .frames
        .get(path)
        .ok_or_else(|| format!("missing trace frame {path:?}"))?;
    let object = vm_trace
        .as_object()
        .ok_or_else(|| format!("vmTrace for frame {path:?} must be an object"))?;
    let ops = object
        .get("ops")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("vmTrace for frame {path:?} is missing ops"))?;
    let bytecode = if ops.iter().any(|step| step.get("op").is_none()) {
        let code = object
            .get("code")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("vmTrace for frame {path:?} is missing code"))?;
        let decoded = decode_hex(code)?;
        if decoded.len() > MAX_MEMORY {
            return Err(format!("vmTrace bytecode exceeds {MAX_MEMORY} bytes"));
        }
        Some(decoded)
    } else {
        None
    };

    let direct_children = tree.children.get(path).map(Vec::as_slice).unwrap_or(&[]);
    let mut child_cursor = 0usize;
    let mut stack: Vec<Word> = Vec::new();
    let mut memory: Vec<u8> = Vec::new();
    let mut logs = Vec::new();

    for (step_index, step) in ops.iter().enumerate() {
        budget.steps = budget
            .steps
            .checked_add(1)
            .ok_or_else(|| "vmTrace step count overflow".to_string())?;
        if budget.steps > MAX_STEPS {
            return Err(format!("vmTrace exceeds maximum {MAX_STEPS} steps"));
        }

        let sub = step
            .get("sub")
            .ok_or_else(|| format!("vmTrace step {step_index} in frame {path:?} is missing sub"))?;
        let ex_value = step
            .get("ex")
            .ok_or_else(|| format!("vmTrace step {step_index} in frame {path:?} is missing ex"))?;
        if ex_value.is_null() {
            if !frame.failed {
                return Err(format!(
                    "successful trace frame {path:?} contains a null ex at step {step_index}"
                ));
            }
            if step_index + 1 != ops.len() || !sub.is_null() {
                return Err(format!(
                    "faulting null ex must be the final step in trace frame {path:?}"
                ));
            }
            break;
        }

        let pc = step
            .get("pc")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("vmTrace step {step_index} in frame {path:?} has invalid pc"))?;
        let pc = usize::try_from(pc)
            .map_err(|_| format!("vmTrace pc is too large in frame {path:?}"))?;
        let spec = match step.get("op") {
            Some(Value::String(name)) => opcode_from_name(name)?,
            Some(_) => {
                return Err(format!(
                    "vmTrace step {step_index} in frame {path:?} has invalid op"
                ));
            }
            None => {
                let code = bytecode.as_ref().expect("bytecode loaded for missing op");
                let byte = *code.get(pc).ok_or_else(|| {
                    format!("vmTrace pc {pc} is outside bytecode in frame {path:?}")
                })?;
                opcode_from_byte(byte)?
            }
        };

        if !matches!(spec.kind, OpKind::Call(_)) && !sub.is_null() {
            return Err(format!(
                "non-call opcode at step {step_index} in frame {path:?} has a subtrace"
            ));
        }

        let ex = ex_value.as_object().ok_or_else(|| {
            format!("vmTrace ex at step {step_index} in frame {path:?} must be an object")
        })?;
        let pushed = ex
            .get("push")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!("vmTrace ex.push at step {step_index} in frame {path:?} must be an array")
            })?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| format!("invalid stack word in frame {path:?}"))
                    .and_then(parse_word)
            })
            .collect::<Result<Vec<_>, _>>()?;

        if stack.len() < spec.pops {
            return Err(format!(
                "stack underflow at step {step_index} in frame {path:?}: need {}, have {}",
                spec.pops,
                stack.len()
            ));
        }
        validate_pushes(spec, &stack, &pushed, step_index, path)?;

        if let OpKind::Log(topic_count) = spec.kind {
            budget.logs = budget
                .logs
                .checked_add(1)
                .ok_or_else(|| "vmTrace log count overflow".to_string())?;
            if budget.logs > MAX_LOGS {
                return Err(format!("vmTrace exceeds maximum {MAX_LOGS} logs"));
            }
            let size = word_to_usize(&stack[stack.len() - 2], "log memory size")?;
            let data = if size == 0 {
                Vec::new()
            } else {
                if size > MAX_MEMORY {
                    return Err(format!("log memory size exceeds {MAX_MEMORY} bytes"));
                }
                let offset = word_to_usize(&stack[stack.len() - 1], "log memory offset")?;
                let end = offset
                    .checked_add(size)
                    .ok_or_else(|| "log memory range overflow".to_string())?;
                if end > MAX_MEMORY {
                    return Err(format!("log memory range exceeds {MAX_MEMORY} bytes"));
                }
                let mut data = vec![0u8; size];
                if offset < memory.len() {
                    let available_end = end.min(memory.len());
                    data[..available_end - offset].copy_from_slice(&memory[offset..available_end]);
                }
                data
            };
            let mut topics = Vec::with_capacity(topic_count);
            for topic_index in 0..topic_count {
                topics.push(word_hex(&stack[stack.len() - 3 - topic_index]));
            }
            if !frame.failed {
                let address = context.as_ref().ok_or_else(|| {
                    format!("successful trace frame {path:?} has no execution address")
                })?;
                logs.push(json!({
                    "address": address,
                    "topics": topics,
                    "data": bytes_hex(&data),
                }));
            }
        }

        if let OpKind::Call(mode) = spec.kind {
            let target = call_target(mode, &stack)?;
            let candidate = direct_children.get(child_cursor).and_then(|child_path| {
                tree.frames
                    .get(child_path.as_slice())
                    .map(|frame| (child_path, frame))
            });
            let matched =
                candidate.is_some_and(|(_, child)| child_matches(mode, target.as_deref(), child));

            if !matched {
                if vm_sub_has_steps(sub, path, step_index)? {
                    return Err(format!(
                        "call/create opcode at step {step_index} in frame {path:?} has no matching trace frame"
                    ));
                }
            } else {
                let (child_path, child_frame) = candidate.expect("matched child exists");
                child_cursor += 1;
                validate_child_kind(mode, child_frame, child_path)?;
                let child_context = execution_context(mode, child_frame, context.as_ref())?;
                if sub.is_null() {
                    if tree
                        .children
                        .get(child_path)
                        .is_some_and(|children| !children.is_empty())
                    {
                        return Err(format!(
                            "trace frame {child_path:?} has children but no vmTrace sub"
                        ));
                    }
                } else {
                    let mut child_logs =
                        process_frame(sub, child_path, child_context, tree, budget, depth + 1)?;
                    logs.append(&mut child_logs);
                }
            }
        }

        if spec.kind == OpKind::SelfDestruct {
            let child_path = direct_children.get(child_cursor).ok_or_else(|| {
                format!(
                    "SELFDESTRUCT at step {step_index} in frame {path:?} has no suicide trace frame"
                )
            })?;
            let child = tree
                .frames
                .get(child_path.as_slice())
                .ok_or_else(|| format!("missing suicide trace frame {child_path:?}"))?;
            let beneficiary = format!("0x{}", &word_hex(&stack[stack.len() - 1])[26..]);
            match &child.kind {
                TraceKind::Suicide {
                    address,
                    refund_address,
                } if context.as_deref() == Some(address.as_str())
                    && beneficiary == *refund_address => {}
                TraceKind::Suicide { .. } => {
                    return Err(format!(
                        "SELFDESTRUCT context or beneficiary does not match suicide trace frame {child_path:?}"
                    ));
                }
                _ => {
                    return Err(format!(
                        "SELFDESTRUCT mapped to a non-suicide trace frame {child_path:?}"
                    ));
                }
            }
            if tree
                .children
                .get(child_path)
                .is_some_and(|children| !children.is_empty())
            {
                return Err(format!("suicide trace frame {child_path:?} has children"));
            }
            child_cursor += 1;
        }

        stack.truncate(stack.len() - spec.pops);
        stack.extend(pushed);
        if stack.len() > MAX_STACK {
            return Err(format!("stack exceeds EVM maximum {MAX_STACK} words"));
        }
        apply_memory_patch(ex.get("mem"), &mut memory, step_index, path)?;
    }

    if child_cursor != direct_children.len() {
        return Err(format!(
            "trace frame {path:?} has {} unmapped child trace frame(s)",
            direct_children.len() - child_cursor
        ));
    }
    if frame.failed {
        logs.clear();
    }
    Ok(logs)
}

fn call_target(mode: CallMode, stack: &[Word]) -> Result<Option<String>, String> {
    if matches!(mode, CallMode::Create | CallMode::Create2) {
        return Ok(None);
    }
    let word = &stack[stack.len() - 2];
    Ok(Some(format!("0x{}", &word_hex(word)[26..])))
}

fn child_matches(mode: CallMode, target: Option<&str>, child: &Frame) -> bool {
    match (mode, &child.kind) {
        (CallMode::Create | CallMode::Create2, TraceKind::Create { .. }) => true,
        (CallMode::Call, TraceKind::Call { call_type, to }) => {
            call_type == "call" && target == Some(to.as_str())
        }
        (CallMode::CallCode, TraceKind::Call { call_type, to }) => {
            call_type == "callcode" && target == Some(to.as_str())
        }
        (CallMode::DelegateCall, TraceKind::Call { call_type, to }) => {
            call_type == "delegatecall" && target == Some(to.as_str())
        }
        (CallMode::StaticCall, TraceKind::Call { call_type, to }) => {
            call_type == "staticcall" && target == Some(to.as_str())
        }
        _ => false,
    }
}

fn vm_sub_has_steps(sub: &Value, path: &[usize], step_index: usize) -> Result<bool, String> {
    if sub.is_null() {
        return Ok(false);
    }
    sub.get("ops")
        .and_then(Value::as_array)
        .map(|ops| !ops.is_empty())
        .ok_or_else(|| format!("vmTrace sub at step {step_index} in frame {path:?} is missing ops"))
}

fn validate_pushes(
    spec: OpSpec,
    stack: &[Word],
    pushed: &[Word],
    step_index: usize,
    path: &[usize],
) -> Result<(), String> {
    if pushed.len() != spec.pushes {
        return Err(format!(
            "opcode at step {step_index} in frame {path:?} reports {} pushed words, expected {}",
            pushed.len(),
            spec.pushes
        ));
    }
    match spec.kind {
        OpKind::Dup(depth) => {
            let tail = &stack[stack.len() - depth..];
            if pushed[..depth] != *tail || pushed[depth] != tail[0] {
                return Err(format!(
                    "invalid DUP{depth} replacement at step {step_index} in frame {path:?}"
                ));
            }
        }
        OpKind::Swap(depth) => {
            let width = depth + 1;
            let mut expected = stack[stack.len() - width..].to_vec();
            expected.swap(0, width - 1);
            if pushed != expected {
                return Err(format!(
                    "invalid SWAP{depth} replacement at step {step_index} in frame {path:?}"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn execution_context(
    mode: CallMode,
    child: &Frame,
    parent_context: Option<&String>,
) -> Result<Option<String>, String> {
    match mode {
        CallMode::DelegateCall | CallMode::CallCode => Ok(parent_context.cloned()),
        CallMode::Call | CallMode::StaticCall => match &child.kind {
            TraceKind::Call { to, .. } => Ok(Some(to.clone())),
            _ => Err("call opcode mapped to a non-call trace frame".to_string()),
        },
        CallMode::Create | CallMode::Create2 => match &child.kind {
            TraceKind::Create { address } => Ok(address.clone()),
            _ => Err("create opcode mapped to a non-create trace frame".to_string()),
        },
    }
}

fn validate_child_kind(mode: CallMode, child: &Frame, path: &[usize]) -> Result<(), String> {
    match (mode, &child.kind) {
        (CallMode::Create | CallMode::Create2, TraceKind::Create { .. }) => Ok(()),
        (CallMode::Call, TraceKind::Call { call_type, .. }) if call_type == "call" => Ok(()),
        (CallMode::CallCode, TraceKind::Call { call_type, .. }) if call_type == "callcode" => {
            Ok(())
        }
        (CallMode::DelegateCall, TraceKind::Call { call_type, .. })
            if call_type == "delegatecall" =>
        {
            Ok(())
        }
        (CallMode::StaticCall, TraceKind::Call { call_type, .. }) if call_type == "staticcall" => {
            Ok(())
        }
        (_, TraceKind::Other(kind)) => Err(format!(
            "call/create opcode mapped to unsupported {kind} trace frame {path:?}"
        )),
        _ => Err(format!(
            "call/create opcode does not match trace frame {path:?}"
        )),
    }
}

fn apply_memory_patch(
    value: Option<&Value>,
    memory: &mut Vec<u8>,
    step_index: usize,
    path: &[usize],
) -> Result<(), String> {
    let value = value.ok_or_else(|| {
        format!("vmTrace ex.mem at step {step_index} in frame {path:?} is missing")
    })?;
    if value.is_null() {
        return Ok(());
    }
    let patch = value.as_object().ok_or_else(|| {
        format!("vmTrace ex.mem at step {step_index} in frame {path:?} must be an object")
    })?;
    let offset = patch
        .get("off")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("invalid memory offset in frame {path:?}"))?;
    let offset = usize::try_from(offset).map_err(|_| "memory offset is too large".to_string())?;
    let data = patch
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("invalid memory data in frame {path:?}"))
        .and_then(decode_hex)?;
    let end = offset
        .checked_add(data.len())
        .ok_or_else(|| "memory patch range overflow".to_string())?;
    if end > MAX_MEMORY {
        return Err(format!("memory patch exceeds {MAX_MEMORY} bytes"));
    }
    if memory.len() < end {
        memory.resize(end, 0);
    }
    memory[offset..end].copy_from_slice(&data);
    Ok(())
}

fn parse_trace_address(trace: &Value) -> Result<Vec<usize>, String> {
    trace
        .get("traceAddress")
        .and_then(Value::as_array)
        .ok_or_else(|| "trace frame is missing traceAddress".to_string())?
        .iter()
        .map(|part| {
            part.as_u64()
                .ok_or_else(|| "traceAddress must contain unsigned integers".to_string())
                .and_then(|part| {
                    usize::try_from(part).map_err(|_| "traceAddress index is too large".to_string())
                })
        })
        .collect()
}

fn normalize_address(value: &str) -> Result<String, String> {
    let raw = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| format!("invalid address {value}"))?;
    if raw.len() != 40 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("invalid address {value}"));
    }
    Ok(format!("0x{}", raw.to_ascii_lowercase()))
}

fn parse_word(value: &str) -> Result<Word, String> {
    let raw = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| format!("invalid stack word {value}"))?;
    if raw.len() > 64 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("invalid stack word {value}"));
    }
    let padded;
    let raw = if raw.len() % 2 == 1 {
        padded = format!("0{raw}");
        padded.as_str()
    } else {
        raw
    };
    let bytes = decode_raw_hex(raw)?;
    let mut word = [0u8; 32];
    word[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(Word(word))
}

fn word_to_usize(word: &Word, label: &str) -> Result<usize, String> {
    let mut value = 0usize;
    for byte in word.0 {
        value = value
            .checked_mul(256)
            .and_then(|value| value.checked_add(byte as usize))
            .ok_or_else(|| format!("{label} is too large"))?;
    }
    Ok(value)
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let raw = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| "hex value must have 0x prefix".to_string())?;
    if raw.len() % 2 != 0 {
        return Err("hex byte string must have even length".to_string());
    }
    decode_raw_hex(raw)
}

fn decode_raw_hex(raw: &str) -> Result<Vec<u8>, String> {
    raw.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("invalid hex character".to_string()),
    }
}

fn bytes_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(2 + bytes.len() * 2);
    output.push_str("0x");
    for byte in bytes {
        output.push(char::from_digit((byte >> 4) as u32, 16).expect("hex nibble"));
        output.push(char::from_digit((byte & 0x0f) as u32, 16).expect("hex nibble"));
    }
    output
}

fn word_hex(word: &Word) -> String {
    bytes_hex(&word.0)
}

fn spec(pops: usize, pushes: usize) -> OpSpec {
    OpSpec {
        pops,
        pushes,
        kind: OpKind::Normal,
    }
}

fn opcode_from_name(name: &str) -> Result<OpSpec, String> {
    let name = name.to_ascii_uppercase();
    if let Some(depth) = name
        .strip_prefix("DUP")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if (1..=16).contains(&depth) {
            return Ok(OpSpec {
                pops: depth,
                pushes: depth + 1,
                kind: OpKind::Dup(depth),
            });
        }
    }
    if let Some(depth) = name
        .strip_prefix("SWAP")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if (1..=16).contains(&depth) {
            return Ok(OpSpec {
                pops: depth + 1,
                pushes: depth + 1,
                kind: OpKind::Swap(depth),
            });
        }
    }
    if let Some(topics) = name
        .strip_prefix("LOG")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if topics <= 4 {
            return Ok(OpSpec {
                pops: topics + 2,
                pushes: 0,
                kind: OpKind::Log(topics),
            });
        }
    }
    if name == "PUSH0" {
        return Ok(spec(0, 1));
    }
    if let Some(width) = name
        .strip_prefix("PUSH")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if (1..=32).contains(&width) {
            return Ok(spec(0, 1));
        }
    }

    let result = match name.as_str() {
        "STOP" | "JUMPDEST" | "INVALID" => spec(0, 0),
        "ADDRESS" | "ORIGIN" | "CALLER" | "CALLVALUE" | "CALLDATASIZE" | "CODESIZE"
        | "GASPRICE" | "RETURNDATASIZE" | "COINBASE" | "TIMESTAMP" | "NUMBER" | "DIFFICULTY"
        | "PREVRANDAO" | "GASLIMIT" | "CHAINID" | "SELFBALANCE" | "BASEFEE" | "BLOBBASEFEE"
        | "PC" | "MSIZE" | "GAS" => spec(0, 1),
        "ISZERO" | "NOT" | "BALANCE" | "CALLDATALOAD" | "EXTCODESIZE" | "EXTCODEHASH"
        | "BLOCKHASH" | "BLOBHASH" | "MLOAD" | "SLOAD" | "TLOAD" => spec(1, 1),
        "ADD" | "MUL" | "SUB" | "DIV" | "SDIV" | "MOD" | "SMOD" | "EXP" | "SIGNEXTEND" | "LT"
        | "GT" | "SLT" | "SGT" | "EQ" | "AND" | "OR" | "XOR" | "BYTE" | "SHL" | "SHR" | "SAR" => {
            spec(2, 1)
        }
        "ADDMOD" | "MULMOD" => spec(3, 1),
        "KECCAK256" | "SHA3" => spec(2, 1),
        "CALLDATACOPY" | "CODECOPY" | "RETURNDATACOPY" | "MCOPY" => spec(3, 0),
        "EXTCODECOPY" => spec(4, 0),
        "POP" | "JUMP" => spec(1, 0),
        "SELFDESTRUCT" | "SUICIDE" => OpSpec {
            pops: 1,
            pushes: 0,
            kind: OpKind::SelfDestruct,
        },
        "MSTORE" | "MSTORE8" | "SSTORE" | "JUMPI" | "TSTORE" | "RETURN" | "REVERT" => spec(2, 0),
        "CREATE" => OpSpec {
            pops: 3,
            pushes: 1,
            kind: OpKind::Call(CallMode::Create),
        },
        "CALL" => OpSpec {
            pops: 7,
            pushes: 1,
            kind: OpKind::Call(CallMode::Call),
        },
        "CALLCODE" => OpSpec {
            pops: 7,
            pushes: 1,
            kind: OpKind::Call(CallMode::CallCode),
        },
        "DELEGATECALL" => OpSpec {
            pops: 6,
            pushes: 1,
            kind: OpKind::Call(CallMode::DelegateCall),
        },
        "CREATE2" => OpSpec {
            pops: 4,
            pushes: 1,
            kind: OpKind::Call(CallMode::Create2),
        },
        "STATICCALL" => OpSpec {
            pops: 6,
            pushes: 1,
            kind: OpKind::Call(CallMode::StaticCall),
        },
        _ => return Err(format!("unknown opcode {name}")),
    };
    Ok(result)
}

fn opcode_from_byte(byte: u8) -> Result<OpSpec, String> {
    match byte {
        0x00 => opcode_from_name("STOP"),
        0x01 => opcode_from_name("ADD"),
        0x02 => opcode_from_name("MUL"),
        0x03 => opcode_from_name("SUB"),
        0x04 => opcode_from_name("DIV"),
        0x05 => opcode_from_name("SDIV"),
        0x06 => opcode_from_name("MOD"),
        0x07 => opcode_from_name("SMOD"),
        0x08 => opcode_from_name("ADDMOD"),
        0x09 => opcode_from_name("MULMOD"),
        0x0a => opcode_from_name("EXP"),
        0x0b => opcode_from_name("SIGNEXTEND"),
        0x10 => opcode_from_name("LT"),
        0x11 => opcode_from_name("GT"),
        0x12 => opcode_from_name("SLT"),
        0x13 => opcode_from_name("SGT"),
        0x14 => opcode_from_name("EQ"),
        0x15 => opcode_from_name("ISZERO"),
        0x16 => opcode_from_name("AND"),
        0x17 => opcode_from_name("OR"),
        0x18 => opcode_from_name("XOR"),
        0x19 => opcode_from_name("NOT"),
        0x1a => opcode_from_name("BYTE"),
        0x1b => opcode_from_name("SHL"),
        0x1c => opcode_from_name("SHR"),
        0x1d => opcode_from_name("SAR"),
        0x20 => opcode_from_name("KECCAK256"),
        0x30 => opcode_from_name("ADDRESS"),
        0x31 => opcode_from_name("BALANCE"),
        0x32 => opcode_from_name("ORIGIN"),
        0x33 => opcode_from_name("CALLER"),
        0x34 => opcode_from_name("CALLVALUE"),
        0x35 => opcode_from_name("CALLDATALOAD"),
        0x36 => opcode_from_name("CALLDATASIZE"),
        0x37 => opcode_from_name("CALLDATACOPY"),
        0x38 => opcode_from_name("CODESIZE"),
        0x39 => opcode_from_name("CODECOPY"),
        0x3a => opcode_from_name("GASPRICE"),
        0x3b => opcode_from_name("EXTCODESIZE"),
        0x3c => opcode_from_name("EXTCODECOPY"),
        0x3d => opcode_from_name("RETURNDATASIZE"),
        0x3e => opcode_from_name("RETURNDATACOPY"),
        0x3f => opcode_from_name("EXTCODEHASH"),
        0x40 => opcode_from_name("BLOCKHASH"),
        0x41 => opcode_from_name("COINBASE"),
        0x42 => opcode_from_name("TIMESTAMP"),
        0x43 => opcode_from_name("NUMBER"),
        0x44 => opcode_from_name("PREVRANDAO"),
        0x45 => opcode_from_name("GASLIMIT"),
        0x46 => opcode_from_name("CHAINID"),
        0x47 => opcode_from_name("SELFBALANCE"),
        0x48 => opcode_from_name("BASEFEE"),
        0x49 => opcode_from_name("BLOBHASH"),
        0x4a => opcode_from_name("BLOBBASEFEE"),
        0x50 => opcode_from_name("POP"),
        0x51 => opcode_from_name("MLOAD"),
        0x52 => opcode_from_name("MSTORE"),
        0x53 => opcode_from_name("MSTORE8"),
        0x54 => opcode_from_name("SLOAD"),
        0x55 => opcode_from_name("SSTORE"),
        0x56 => opcode_from_name("JUMP"),
        0x57 => opcode_from_name("JUMPI"),
        0x58 => opcode_from_name("PC"),
        0x59 => opcode_from_name("MSIZE"),
        0x5a => opcode_from_name("GAS"),
        0x5b => opcode_from_name("JUMPDEST"),
        0x5c => opcode_from_name("TLOAD"),
        0x5d => opcode_from_name("TSTORE"),
        0x5e => opcode_from_name("MCOPY"),
        0x5f => opcode_from_name("PUSH0"),
        0x60..=0x7f => Ok(spec(0, 1)),
        0x80..=0x8f => opcode_from_name(&format!("DUP{}", byte - 0x7f)),
        0x90..=0x9f => opcode_from_name(&format!("SWAP{}", byte - 0x8f)),
        0xa0..=0xa4 => opcode_from_name(&format!("LOG{}", byte - 0xa0)),
        0xf0 => opcode_from_name("CREATE"),
        0xf1 => opcode_from_name("CALL"),
        0xf2 => opcode_from_name("CALLCODE"),
        0xf3 => opcode_from_name("RETURN"),
        0xf4 => opcode_from_name("DELEGATECALL"),
        0xf5 => opcode_from_name("CREATE2"),
        0xfa => opcode_from_name("STATICCALL"),
        0xfd => opcode_from_name("REVERT"),
        0xfe => opcode_from_name("INVALID"),
        0xff => opcode_from_name("SELFDESTRUCT"),
        _ => Err(format!("unknown opcode byte 0x{byte:02x}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROOT: &str = "0x1111111111111111111111111111111111111111";
    const LIBRARY: &str = "0x2222222222222222222222222222222222222222";
    const CALLEE: &str = "0x3333333333333333333333333333333333333333";
    const CREATED: &str = "0x4444444444444444444444444444444444444444";

    fn root_trace(error: Option<&str>) -> Value {
        let mut trace = json!({
            "action": {"callType":"call", "from":"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "to":ROOT},
            "result": {"gasUsed":"0x1", "output":"0x"},
            "subtraces":0,
            "traceAddress":[],
            "type":"call"
        });
        if let Some(error) = error {
            trace["error"] = json!(error);
        }
        trace
    }

    fn child_call(index: usize, call_type: &str, to: &str, error: Option<&str>) -> Value {
        let mut trace = json!({
            "action": {"callType":call_type, "from":ROOT, "to":to},
            "result": {"gasUsed":"0x1", "output":"0x"},
            "subtraces":0,
            "traceAddress":[index],
            "type":"call"
        });
        if let Some(error) = error {
            trace["error"] = json!(error);
        }
        trace
    }

    fn op(name: &str, push: &[&str]) -> Value {
        json!({
            "pc":0,
            "op":name,
            "ex":{"push":push, "mem":null},
            "sub":null
        })
    }

    fn log1_ops(topic: &str, byte: u8) -> Vec<Value> {
        let data = format!("0x{}", format!("{byte:02x}").repeat(32));
        vec![
            op("PUSH1", &["0x1"]),
            op("PUSH1", &["0x0"]),
            json!({
                "pc":0,
                "op":"MSTORE",
                "ex":{"push":[], "mem":{"off":0,"data":data}},
                "sub":null
            }),
            op("PUSH32", &[topic]),
            op("PUSH1", &["0x20"]),
            op("PUSH1", &["0x0"]),
            op("LOG1", &[]),
        ]
    }

    fn call_op(
        name: &str,
        arguments: usize,
        target: Option<&str>,
        success: bool,
        sub: Value,
    ) -> Vec<Value> {
        let mut words = vec!["0x0"; arguments];
        if let Some(target) = target {
            words[arguments - 2] = target;
        }
        let mut ops = words
            .into_iter()
            .map(|word| op("PUSH32", &[word]))
            .collect::<Vec<_>>();
        ops.push(json!({
            "pc":0,
            "op":name,
            "ex":{"push":[if success {"0x1"} else {"0x0"}], "mem":null},
            "sub":sub
        }));
        ops
    }

    fn vm(ops: Vec<Value>) -> Value {
        json!({"code":"0x", "ops":ops})
    }

    fn decoded_fixture(response: &str) -> Vec<Value> {
        let fixture: Value = serde_json::from_str(response).unwrap();
        let mut actual = Vec::new();
        for result in fixture["result"].as_array().unwrap() {
            actual.extend(
                extract_logs(&result["vmTrace"], result["trace"].as_array().unwrap()).unwrap(),
            );
        }
        actual
    }

    #[test]
    fn decodes_recorded_usdt_events() {
        let expected: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/vm_trace_usdt_expected.json"
        ))
        .unwrap();

        assert_eq!(
            decoded_fixture(include_str!(
                "../../../tests/fixtures/vm_trace_usdt_response.json"
            )),
            *expected.as_array().unwrap()
        );
    }

    #[test]
    fn matches_call_tracer_for_real_nested_calls_with_a_caught_revert() {
        let expected: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/vm_trace_nested_expected.json"
        ))
        .unwrap();
        assert_eq!(
            decoded_fixture(include_str!(
                "../../../tests/fixtures/vm_trace_nested_response.json"
            )),
            *expected.as_array().unwrap()
        );
    }

    #[test]
    fn maps_real_precompile_and_empty_code_calls_before_a_logged_child() {
        let expected: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/vm_trace_precompile_expected.json"
        ))
        .unwrap();
        assert_eq!(
            decoded_fixture(include_str!(
                "../../../tests/fixtures/vm_trace_precompile_response.json"
            )),
            *expected.as_array().unwrap()
        );
    }

    #[test]
    fn decodes_log_zero_through_four_in_topic_order() {
        let mut ops = vec![json!({
            "pc":0,
            "op":"PUSH1",
            "ex":{"push":["0x1"], "mem":{"off":0,"data":"0xab"}},
            "sub":null
        })];
        // Remove the setup value, leaving only the memory patch behind.
        ops.push(op("POP", &[]));
        for topic_count in 0..=4 {
            for topic in (1..=topic_count).rev() {
                ops.push(op("PUSH1", &[&format!("0x{topic:x}")]));
            }
            ops.push(op("PUSH1", &["0x1"]));
            ops.push(op("PUSH1", &["0x0"]));
            ops.push(op(&format!("LOG{topic_count}"), &[]));
        }

        let logs = extract_logs(&vm(ops), &[root_trace(None)]).unwrap();
        assert_eq!(logs.len(), 5);
        assert_eq!(logs[0], json!({"address":ROOT,"topics":[],"data":"0xab"}));
        assert_eq!(
            logs[1]["topics"],
            json!(["0x0000000000000000000000000000000000000000000000000000000000000001"])
        );
        assert_eq!(
            logs[4]["topics"],
            json!([
                "0x0000000000000000000000000000000000000000000000000000000000000001",
                "0x0000000000000000000000000000000000000000000000000000000000000002",
                "0x0000000000000000000000000000000000000000000000000000000000000003",
                "0x0000000000000000000000000000000000000000000000000000000000000004"
            ])
        );
    }

    #[test]
    fn zero_length_log_accepts_an_arbitrary_256_bit_memory_offset() {
        let vm_trace = vm(vec![
            op("PUSH0", &["0x0"]),
            op(
                "PUSH32",
                &["0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"],
            ),
            op("LOG0", &[]),
        ]);

        assert_eq!(
            extract_logs(&vm_trace, &[root_trace(None)]).unwrap(),
            vec![json!({"address":ROOT,"topics":[],"data":"0x"})]
        );
    }

    #[test]
    fn rejects_oversized_log_data_before_allocating_it() {
        let vm_trace = vm(vec![
            op("PUSH8", &["0x7fffffffffffffff"]),
            op("PUSH0", &["0x0"]),
            op("LOG0", &[]),
        ]);

        let error = extract_logs(&vm_trace, &[root_trace(None)]).unwrap_err();
        assert!(
            error.contains("log memory size"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn preserves_order_uses_delegate_context_and_discards_caught_revert_logs() {
        let successful_child = vm(log1_ops("0x2", 0x22));
        let reverted_child = vm(log1_ops("0x3", 0x33));

        let mut root_ops = log1_ops("0x1", 0x11);
        root_ops.extend(call_op(
            "DELEGATECALL",
            6,
            Some(LIBRARY),
            true,
            successful_child,
        ));
        root_ops.extend(call_op("CALL", 7, Some(CALLEE), false, reverted_child));
        root_ops.extend(log1_ops("0x4", 0x44));

        let logs = extract_logs(
            &vm(root_ops),
            &[
                root_trace(None),
                child_call(0, "delegatecall", LIBRARY, None),
                child_call(1, "call", CALLEE, Some("Reverted")),
            ],
        )
        .unwrap();

        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0]["address"], ROOT);
        assert_eq!(logs[1]["address"], ROOT);
        assert_eq!(logs[2]["address"], ROOT);
        assert_eq!(logs[0]["topics"][0], format!("0x{:064x}", 1));
        assert_eq!(logs[1]["topics"][0], format!("0x{:064x}", 2));
        assert_eq!(logs[2]["topics"][0], format!("0x{:064x}", 4));
    }

    #[test]
    fn uses_created_contract_as_log_address() {
        let child = vm(log1_ops("0x9", 0x99));
        let root_vm = vm(call_op("CREATE", 3, None, true, child));
        let create_trace = json!({
            "action":{"from":ROOT,"gas":"0x100", "init":"0x", "value":"0x0"},
            "result":{"address":CREATED,"code":"0x", "gasUsed":"0x1"},
            "subtraces":0,
            "traceAddress":[0],
            "type":"create"
        });

        let logs = extract_logs(&root_vm, &[root_trace(None), create_trace]).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0]["address"], CREATED);
    }

    #[test]
    fn uses_parent_execution_address_for_callcode_logs() {
        let child = vm(log1_ops("0xa", 0xaa));
        let root_vm = vm(call_op("CALLCODE", 7, Some(LIBRARY), true, child));

        let logs = extract_logs(
            &root_vm,
            &[root_trace(None), child_call(0, "callcode", LIBRARY, None)],
        )
        .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0]["address"], ROOT);
    }

    #[test]
    fn derives_missing_opcode_names_from_bytecode() {
        let vm_trace = json!({
            "code":"0x600160005360016000a0",
            "ops":[
                {"pc":0,"ex":{"push":["0x1"],"mem":null},"sub":null},
                {"pc":2,"ex":{"push":["0x0"],"mem":null},"sub":null},
                {"pc":4,"ex":{"push":[],"mem":{"off":0,"data":"0x01"}},"sub":null},
                {"pc":5,"ex":{"push":["0x1"],"mem":null},"sub":null},
                {"pc":7,"ex":{"push":["0x0"],"mem":null},"sub":null},
                {"pc":9,"ex":{"push":[],"mem":null},"sub":null}
            ]
        });

        assert_eq!(
            extract_logs(&vm_trace, &[root_trace(None)]).unwrap(),
            vec![json!({"address":ROOT,"topics":[],"data":"0x01"})]
        );
    }

    #[test]
    fn root_failure_returns_no_logs_even_when_vm_trace_is_missing_or_malformed() {
        assert_eq!(
            extract_logs(&Value::Null, &[root_trace(Some("Reverted"))]).unwrap(),
            Vec::<Value>::new()
        );
        assert_eq!(
            extract_logs(
                &json!({"ops":[{"op":"NOT_AN_OPCODE"}]}),
                &[root_trace(Some("Out of gas"))]
            )
            .unwrap(),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn rejects_unknown_opcodes_stack_underflow_and_null_ex_on_success() {
        let cases = [
            vm(vec![op("NOT_AN_OPCODE", &[])]),
            vm(vec![op("ADD", &["0x0"])]),
            json!({"code":"0x00","ops":[{"pc":0,"op":"STOP","ex":null,"sub":null}]}),
        ];

        for malformed in cases {
            assert!(extract_logs(&malformed, &[root_trace(None)]).is_err());
        }
    }

    #[test]
    fn rejects_call_frames_that_cannot_be_mapped_to_trace_addresses() {
        let vm_trace = vm(call_op(
            "CALL",
            7,
            Some(CALLEE),
            true,
            vm(log1_ops("0x1", 0x11)),
        ));
        let error = extract_logs(&vm_trace, &[root_trace(None)]).unwrap_err();
        assert!(error.contains("trace frame"), "unexpected error: {error}");
    }

    #[test]
    fn tolerates_a_final_null_ex_only_in_a_failed_child_frame() {
        let child = json!({
            "code":"0xfe",
            "ops":[{"pc":0,"op":"INVALID","ex":null,"sub":null}]
        });
        let root_vm = vm(call_op("CALL", 7, Some(CALLEE), false, child));

        assert_eq!(
            extract_logs(
                &root_vm,
                &[
                    root_trace(None),
                    child_call(0, "call", CALLEE, Some("Bad instruction"))
                ]
            )
            .unwrap(),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn caught_undefined_opcode_fault_does_not_discard_parent_logs() {
        let child = json!({
            "code":"0x0c",
            "ops":[{"pc":0,"ex":null,"sub":null}]
        });
        let mut root_ops = log1_ops("0x1", 0x11);
        root_ops.extend(call_op("CALL", 7, Some(CALLEE), false, child));
        root_ops.extend(log1_ops("0x2", 0x22));

        let logs = extract_logs(
            &vm(root_ops),
            &[
                root_trace(None),
                child_call(0, "call", CALLEE, Some("Bad instruction")),
            ],
        )
        .unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0]["topics"][0], format!("0x{:064x}", 1));
        assert_eq!(logs[1]["topics"][0], format!("0x{:064x}", 2));
    }

    #[test]
    fn consumes_and_validates_parity_suicide_child_trace() {
        let beneficiary = "0x5555555555555555555555555555555555555555";
        let vm_trace = vm(vec![op("PUSH32", &[beneficiary]), op("SELFDESTRUCT", &[])]);
        let suicide = json!({
            "action":{"address":ROOT,"refundAddress":beneficiary,"balance":"0x1"},
            "traceAddress":[0],
            "type":"suicide"
        });

        assert_eq!(
            extract_logs(&vm_trace, &[root_trace(None), suicide]).unwrap(),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn rejects_memory_patches_over_the_limit() {
        let vm_trace = vm(vec![json!({
            "pc":0,
            "op":"PUSH0",
            "ex":{"push":["0x0"],"mem":{"off":33_554_432,"data":"0x01"}},
            "sub":null
        })]);
        let error = extract_logs(&vm_trace, &[root_trace(None)]).unwrap_err();
        assert!(error.contains("memory"), "unexpected error: {error}");
    }
}
