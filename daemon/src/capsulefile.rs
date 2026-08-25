//! Human-authored Capsulefile parsing for the Pack builder.
//!
//! Capsulefile deliberately is not a shell language.  It describes one
//! immutable Capsule contract which the builder later serializes to the legacy
//! catalog-compatible manifest/profile JSON pair.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct CapsulefileSpec {
    pub runtime_source: String,
    pub name: String,
    pub version: String,
    pub profile: Value,
}

pub fn parse(source: &str) -> Result<CapsulefileSpec, Error> {
    let lines = logical_lines(source)?;
    let mut runtime_source = None;
    let mut identity = None;
    let mut inputs = Vec::new();
    let mut input_names = BTreeSet::new();
    let mut output = None;
    let mut argv = None;
    let mut limits = None;
    let mut allowed_overrides = None;

    for (line_number, line) in lines {
        let tokens = line.split_whitespace().collect::<Vec<_>>();
        let Some((directive, arguments)) = tokens.split_first() else {
            continue;
        };
        match *directive {
            "FROM" => {
                require_once(&runtime_source, "FROM", line_number)?;
                if arguments.len() != 1 || !arguments[0].starts_with("runtime://") {
                    return invalid(
                        line_number,
                        "FROM은 runtime://<publisher>/<package>:<version> 하나여야 합니다",
                    );
                }
                runtime_source = Some(arguments[0].to_owned());
            }
            "CAPSULE" => {
                require_once(&identity, "CAPSULE", line_number)?;
                if arguments.len() != 1 {
                    return invalid(line_number, "CAPSULE은 <name>@<version> 하나여야 합니다");
                }
                let (name, version) = arguments[0].rsplit_once('@').ok_or_else(|| {
                    Error::InvalidArgument(format!(
                        "Capsulefile:{line_number}: CAPSULE은 <name>@<version> 형식이어야 합니다"
                    ))
                })?;
                if name.is_empty() || version.is_empty() {
                    return invalid(line_number, "CAPSULE identity는 비어 있을 수 없습니다");
                }
                identity = Some((name.to_owned(), version.to_owned()));
            }
            "INPUT" => {
                if arguments.len() != 2 || arguments[1] != "ARTIFACT" {
                    return invalid(line_number, "INPUT은 <name> ARTIFACT 형식이어야 합니다");
                }
                let name = arguments[0];
                if !input_names.insert(name.to_owned()) {
                    return invalid(line_number, "INPUT 또는 OPTION 이름이 중복되었습니다");
                }
                inputs.push(json!({"name": name, "kind":"LOCAL_INPUT", "required":true}));
            }
            "OPTION" => {
                let option = parse_int_option(arguments, line_number)?;
                let name = option["name"].as_str().expect("builder created name");
                if !input_names.insert(name.to_owned()) {
                    return invalid(line_number, "INPUT 또는 OPTION 이름이 중복되었습니다");
                }
                inputs.push(option);
            }
            "OUTPUT" => {
                require_once(&output, "OUTPUT", line_number)?;
                output = Some(parse_output(arguments, line_number)?);
            }
            "COMMAND" => {
                require_once(&argv, "COMMAND", line_number)?;
                if arguments.is_empty() {
                    return invalid(line_number, "COMMAND는 하나 이상의 argv token이 필요합니다");
                }
                argv = Some(arguments.iter().map(|token| (*token).to_owned()).collect());
            }
            "LIMIT" => {
                require_once(&limits, "LIMIT", line_number)?;
                limits = Some(parse_limits(arguments, line_number)?);
            }
            "ALLOW" => {
                require_once(&allowed_overrides, "ALLOW", line_number)?;
                allowed_overrides = Some(parse_allowed_overrides(arguments, line_number)?);
            }
            _ => return invalid(line_number, "지원하지 않는 directive입니다"),
        }
    }

    let runtime_source = required(runtime_source, "FROM")?;
    let (name, version) = required(identity, "CAPSULE")?;
    let output = required(output, "OUTPUT")?;
    let raw_argv = required(argv, "COMMAND")?;
    let limits = required(limits, "LIMIT")?;
    let allowed_overrides = allowed_overrides.unwrap_or_default();
    let argv = materialize_argv(raw_argv, &inputs, &output)?;

    Ok(CapsulefileSpec {
        runtime_source,
        name: name.clone(),
        version: version.clone(),
        profile: json!({
            "schemaVersion":"taskcage.profile/v0alpha1",
            "name":name,
            "version":version,
            "inputs":inputs,
            "output":output,
            "argv":argv,
            "policy":{"limits":limits, "output":{"stdoutTailMaxBytes":65536, "stderrTailMaxBytes":65536}},
            "allowedOverrides":allowed_overrides
        }),
    })
}

fn logical_lines(source: &str) -> Result<Vec<(usize, String)>, Error> {
    let mut result = Vec::new();
    let mut pending = String::new();
    let mut start = 0;
    for (offset, raw) in source.lines().enumerate() {
        let line_number = offset + 1;
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if pending.is_empty() {
            start = line_number;
        }
        if let Some(prefix) = line.strip_suffix('\\') {
            pending.push_str(prefix.trim_end());
            pending.push(' ');
            continue;
        }
        pending.push_str(line);
        result.push((start, std::mem::take(&mut pending)));
    }
    if !pending.is_empty() {
        return invalid(
            start,
            "line continuation 뒤에는 다음 COMMAND line이 필요합니다",
        );
    }
    Ok(result)
}

fn parse_int_option(arguments: &[&str], line: usize) -> Result<Value, Error> {
    if arguments.len() != 4 || arguments[1] != "INT" || arguments[2] != "ALLOWED" {
        return invalid(
            line,
            "OPTION은 <name> INT ALLOWED <comma-values> 형식이어야 합니다",
        );
    }
    let allowed = arguments[3]
        .split(',')
        .map(|value| parse_i64(value, line, "OPTION allowed value"))
        .collect::<Result<Vec<_>, _>>()?;
    if allowed.is_empty() || !allowed.windows(2).all(|values| values[0] < values[1]) {
        return invalid(
            line,
            "OPTION allowed values는 오름차순 unique 값이어야 합니다",
        );
    }
    Ok(json!({"name":arguments[0], "kind":"INT64", "required":true, "allowedValues":allowed}))
}

fn parse_output(arguments: &[&str], line: usize) -> Result<Value, Error> {
    if arguments.len() != 7
        || arguments[1] != "FILE"
        || arguments[3] != "MEDIA_TYPE"
        || arguments[5] != "MAX_BYTES"
    {
        return invalid(
            line,
            "OUTPUT은 <name> FILE <file> MEDIA_TYPE <type> MAX_BYTES <bytes> 형식이어야 합니다",
        );
    }
    let maximum_bytes = arguments[6].parse::<u64>().map_err(|_| {
        Error::InvalidArgument(format!(
            "Capsulefile:{line}: OUTPUT MAX_BYTES는 양의 정수여야 합니다"
        ))
    })?;
    if maximum_bytes == 0 || arguments[2].contains('/') || arguments[2].contains("..") {
        return invalid(
            line,
            "OUTPUT file은 단일 파일 이름이고 MAX_BYTES는 양수여야 합니다",
        );
    }
    Ok(
        json!({"name":arguments[0], "fileName":arguments[2], "mediaType":arguments[4], "maximumBytes":maximum_bytes}),
    )
}

fn parse_limits(arguments: &[&str], line: usize) -> Result<Value, Error> {
    if arguments.len() != 8
        || arguments[0] != "CPU"
        || arguments[2] != "MEMORY"
        || arguments[4] != "PIDS"
        || arguments[6] != "TIMEOUT"
    {
        return invalid(
            line,
            "LIMIT은 CPU <cores> MEMORY <size> PIDS <count> TIMEOUT <duration> 형식이어야 합니다",
        );
    }
    let cpu = arguments[1].parse::<u64>().map_err(|_| {
        Error::InvalidArgument(format!("Capsulefile:{line}: CPU는 양의 정수여야 합니다"))
    })?;
    let memory = parse_bytes(arguments[3], line)?;
    let pids = arguments[5].parse::<u64>().map_err(|_| {
        Error::InvalidArgument(format!("Capsulefile:{line}: PIDS는 양의 정수여야 합니다"))
    })?;
    let timeout = parse_millis(arguments[7], line)?;
    if cpu == 0 || pids == 0 {
        return invalid(line, "CPU와 PIDS는 양수여야 합니다");
    }
    Ok(
        json!({"cpuMax":{"quotaMicros":cpu * 100000, "periodMicros":100000}, "memoryMaxBytes":memory, "pidsMax":pids, "wallTimeLimitMs":timeout}),
    )
}

fn parse_allowed_overrides(arguments: &[&str], line: usize) -> Result<Vec<String>, Error> {
    if arguments.len() != 2 || arguments[0] != "OVERRIDE" {
        return invalid(line, "ALLOW는 OVERRIDE <comma-fields> 형식이어야 합니다");
    }
    let mut result = Vec::new();
    for value in arguments[1].split(',') {
        let field = match value {
            "CPU" => "limits.cpuMax",
            "MEMORY" => "limits.memoryMaxBytes",
            "PIDS" => "limits.pidsMax",
            "TIMEOUT" => "limits.wallTimeLimitMs",
            _ => {
                return invalid(
                    line,
                    "ALLOW OVERRIDE는 CPU, MEMORY, PIDS, TIMEOUT만 지원합니다",
                );
            }
        };
        if result.iter().any(|existing| existing == field) {
            return invalid(line, "ALLOW OVERRIDE field가 중복되었습니다");
        }
        result.push(field.to_owned());
    }
    Ok(result)
}

fn materialize_argv(
    raw: Vec<String>,
    inputs: &[Value],
    output: &Value,
) -> Result<Vec<Value>, Error> {
    let input_kinds = inputs
        .iter()
        .filter_map(|input| Some((input.get("name")?.as_str()?, input.get("kind")?.as_str()?)))
        .collect::<Vec<_>>();
    let output_name = output
        .get("name")
        .and_then(Value::as_str)
        .expect("builder created output");
    raw.into_iter()
        .map(|token| {
            if token.contains(['|', ';', '>', '<']) || token.contains("&&") {
                return Err(Error::InvalidArgument(
                    "Capsulefile: COMMAND는 shell token을 포함할 수 없습니다".to_owned(),
                ));
            }
            if let Some(slot) = token
                .strip_prefix("${")
                .and_then(|value| value.strip_suffix('}'))
            {
                if slot == output_name {
                    return Ok(json!({"output":slot}));
                }
                let Some((_, kind)) = input_kinds.iter().find(|(name, _)| *name == slot) else {
                    return Err(Error::InvalidArgument(format!(
                        "Capsulefile: COMMAND placeholder를 찾을 수 없습니다: {slot}"
                    )));
                };
                return Ok(match *kind {
                    "LOCAL_INPUT" => json!({"input":slot}),
                    "INT64" => json!({"int64":slot}),
                    _ => unreachable!("Capsulefile only emits supported inputs"),
                });
            }
            Ok(json!(token))
        })
        .collect()
}

fn parse_i64(value: &str, line: usize, label: &str) -> Result<i64, Error> {
    value.parse().map_err(|_| {
        Error::InvalidArgument(format!("Capsulefile:{line}: {label}는 정수여야 합니다"))
    })
}
fn parse_bytes(value: &str, line: usize) -> Result<u64, Error> {
    value
        .strip_suffix("MiB")
        .and_then(|number| number.parse::<u64>().ok())
        .filter(|number| *number > 0)
        .and_then(|number| number.checked_mul(1024 * 1024))
        .ok_or_else(|| {
            Error::InvalidArgument(format!(
                "Capsulefile:{line}: MEMORY는 양의 <n>MiB여야 합니다"
            ))
        })
}
fn parse_millis(value: &str, line: usize) -> Result<u64, Error> {
    value
        .strip_suffix('m')
        .and_then(|number| number.parse::<u64>().ok())
        .filter(|number| *number > 0)
        .and_then(|number| number.checked_mul(60_000))
        .ok_or_else(|| {
            Error::InvalidArgument(format!(
                "Capsulefile:{line}: TIMEOUT은 양의 <n>m여야 합니다"
            ))
        })
}
fn require_once<T>(value: &Option<T>, name: &str, line: usize) -> Result<(), Error> {
    if value.is_some() {
        invalid(line, &format!("{name} directive가 중복되었습니다"))
    } else {
        Ok(())
    }
}
fn required<T>(value: Option<T>, name: &str) -> Result<T, Error> {
    value.ok_or_else(|| {
        Error::InvalidArgument(format!("Capsulefile: {name} directive가 필요합니다"))
    })
}
fn invalid<T>(line: usize, message: &str) -> Result<T, Error> {
    Err(Error::InvalidArgument(format!(
        "Capsulefile:{line}: {message}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FFMPEG: &str = "FROM runtime://example.org/ffmpeg-runtime:7.1.0\nCAPSULE ffmpeg-audio-to-wav@1.0.0\nINPUT source ARTIFACT\nOPTION sampleRateHz INT ALLOWED 8000,16000,22050\nOUTPUT audio FILE result.wav MEDIA_TYPE audio/wav MAX_BYTES 1024\nCOMMAND -i ${source} -ar ${sampleRateHz} ${audio}\nLIMIT CPU 1 MEMORY 512MiB PIDS 32 TIMEOUT 2m\nALLOW OVERRIDE MEMORY,TIMEOUT\n";

    #[test]
    fn parses_a_single_shell_free_capsule_contract() {
        let spec = parse(FFMPEG).unwrap();
        assert_eq!(
            spec.runtime_source,
            "runtime://example.org/ffmpeg-runtime:7.1.0"
        );
        assert_eq!(spec.name, "ffmpeg-audio-to-wav");
        assert_eq!(
            spec.profile["argv"],
            json!(["-i", {"input":"source"}, "-ar", {"int64":"sampleRateHz"}, {"output":"audio"}])
        );
        assert_eq!(
            spec.profile["allowedOverrides"],
            json!(["limits.memoryMaxBytes", "limits.wallTimeLimitMs"])
        );
    }

    #[test]
    fn rejects_shell_tokens_unknown_placeholders_and_duplicate_directives() {
        assert!(parse(&FFMPEG.replace("-i ${source}", "-i ${missing}")).is_err());
        assert!(parse(&FFMPEG.replace("-i ${source}", "-i ${source} | tee x")).is_err());
        assert!(parse(&format!("{FFMPEG}INPUT source ARTIFACT")).is_err());
    }
}
