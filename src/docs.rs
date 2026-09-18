//! Writes the reference pages from the code they describe.
//!
//! A configuration reference written by hand is wrong the first time a field
//! changes, and wrong documentation is worse than none: it is believed. The
//! schema, the command table, and the character table all already say what
//! they mean, so the pages are made from them and the check fails when what
//! is committed no longer matches.
//!
//! Until the cutover, the committed pages are the TypeScript generator's: the
//! Rust schema carries condensed doc comments, so this module's output differs
//! in prose. The byte check against the committed files activates at the
//! cutover, when these pages become the ones that are committed.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::chat::chars::{ALL_PREFIXES, ReactionKey, all_chars};
use crate::config::schema::defaults;
use crate::config::validate::validate_config;
use crate::session::commands::{COMMANDS, CommandAccess, CommandGroup};

/// One field of one configuration section.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The field's name, as the configuration file spells it.
    pub name: String,
    /// The type it accepts, as the schema declares it.
    pub type_: String,
    /// What its doc comment says, as one paragraph.
    pub says: String,
}

/// One section of the configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    /// The field's name, as the configuration file spells it.
    pub name: String,
    /// What its doc comment says, as one paragraph.
    pub says: String,
    /// Its fields, in the order the schema declares them.
    pub fields: Vec<Field>,
}

/// How far a line opens or closes square brackets, for spanning attributes.
fn bracket_depth(line: &str) -> i32 {
    i32::try_from(line.matches('[').count()).unwrap_or(0)
        - i32::try_from(line.matches(']').count()).unwrap_or(0)
}

/// Reads the interfaces out of the schema.
///
/// A scanner rather than a parser: the file is written to one shape, every
/// field carries a doc comment, and anything it cannot read is a failure
/// rather than a field quietly left out.
pub fn read_schema(source: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut comment: Vec<String> = Vec::new();
    let mut attribute_depth = 0_i32;

    for line in source.lines() {
        let trimmed = line.trim();

        // An attribute may run over several lines, and the lines inside one
        // are not declarations. Counted rather than matched, so a reason
        // string spanning lines does not read as a field.
        if attribute_depth > 0 {
            attribute_depth += bracket_depth(trimmed);
            continue;
        }
        if let Some(doc) = trimmed.strip_prefix("///") {
            comment.push(doc.trim().to_owned());
            continue;
        }
        if trimmed.starts_with("#[") {
            attribute_depth = bracket_depth(trimmed);
            continue;
        }
        if trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }

        let named = trimmed
            .strip_prefix("pub struct ")
            .and_then(|rest| rest.strip_suffix(" {"))
            .map(|name| name.trim().to_owned());
        if let Some(name) = named {
            // The TypeScript interfaces are named for their key (`ChatConfig`),
            // and the pages follow the key rather than the whole name. The
            // bare `Config` is the root, documented as the top level. Any
            // other struct is not a section of the configuration at all.
            let says = std::mem::take(&mut comment).join(" ");
            if name == "Config" || name.ends_with("Config") {
                let key = match name.strip_suffix("Config") {
                    Some("") => "Root",
                    Some(key) => key,
                    None => name.as_str(),
                };
                sections.push(Section {
                    name: key.to_owned(),
                    says,
                    fields: Vec::new(),
                });
            }
            continue;
        }

        if trimmed == "}" {
            comment.clear();
            continue;
        }

        // Fields are the only other `pub` items a section can hold; the
        // constants and functions elsewhere in the file are not settings.
        let body = match trimmed.strip_prefix("pub ") {
            Some(body)
                if !body.starts_with("const ")
                    && !body.starts_with("fn ")
                    && !body.starts_with("mod ")
                    && !body.starts_with("use ")
                    && !body.starts_with("type ")
                    && !body.starts_with("enum ") =>
            {
                body
            }
            _ => {
                comment.clear();
                continue;
            }
        };
        if let Some((name, type_)) = body.split_once(':') {
            let name = name.trim().to_owned();
            let type_ = type_
                .trim()
                .trim_end_matches(',')
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(section) = sections.last_mut() {
                section.fields.push(Field {
                    name: to_camel(&name),
                    type_,
                    says: std::mem::take(&mut comment).join(" "),
                });
            }
        } else if !trimmed.starts_with("//") {
            comment.clear();
        }
    }

    assert!(
        !sections.is_empty(),
        "no interfaces were found in the schema"
    );
    for section in &sections {
        assert!(
            !section.fields.is_empty(),
            "{} was read with no fields",
            section.name
        );
    }
    sections
}

/// Turns a `snake_case` field name into the camelCase key the file uses.
fn to_camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for character in name.chars() {
        if character == '_' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    out
}

/// Spells a Rust type the way the configuration file and the pages name it.
fn shown_type(type_: &str) -> String {
    let bare = type_
        .trim_start_matches("Option<")
        .trim_end_matches('>')
        .trim();
    let optional = type_.starts_with("Option<") && type_.ends_with('>');
    let named = match bare {
        "String" => "string".to_owned(),
        "bool" => "boolean".to_owned(),
        "f64" | "u16" | "u32" | "u64" | "usize" => "number".to_owned(),
        "Vec<String>" => "string[]".to_owned(),
        "Vec<u16>" => "number[]".to_owned(),
        "HashMap<String, String>" | "BTreeMap<String, String>" => {
            "Record<string, string>".to_owned()
        }
        "HashMap<String, Value>" => "Record<string, unknown>".to_owned(),
        other => other.to_owned(),
    };
    if optional {
        format!("{named} | undefined")
    } else {
        named
    }
}

/// Which fields the daemon refuses to start without.
///
/// Asked of the validator rather than decided here: it is the thing that
/// actually refuses, so a field that becomes required says so in the page
/// without anybody remembering to change it.
pub fn required_fields() -> std::collections::BTreeSet<String> {
    let mut named = std::collections::BTreeSet::new();

    let mut collect = |raw: &Value| {
        if let Err(error) = validate_config(raw) {
            for problem in &error.problems {
                let at = problem.find(" is required").unwrap_or(problem.len());
                let subject = problem[..at].trim();
                if subject
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '.' || c == '_')
                {
                    named.insert(subject.to_owned());
                }
            }
        }
    };

    collect(&serde_json::json!({}));
    collect(&serde_json::json!({ "github": {}, "agent": { "delegate": {} } }));
    named
}

/// Renders a value the way JavaScript would, so defaults read as they would
/// from a configuration file.
fn js_value(value: Value) -> String {
    match value {
        Value::Number(number) => {
            let as_f64 = number.as_f64().unwrap_or_default();
            if as_f64.fract() == 0.0 && as_f64.abs() < 1e15 {
                format!("{as_f64:.0}")
            } else {
                format!("{as_f64}")
            }
        }
        other => other.to_string(),
    }
}

/// The default for one field, as it would be written in the file.
fn default_of(section: &str, field: &str) -> String {
    let held: BTreeMap<&str, Value> = defaults_table();
    let key = format!("{}/{}", section.to_lowercase(), field).to_lowercase();
    let value = held.get(key.as_str());
    value.map_or(String::new(), |value| {
        format!("`{}`", js_value(value.clone()))
    })
}

/// The defaults, keyed by `section/field` in lower case.
///
/// Written out rather than reflected, because the constants are named for the
/// code that reads them and the page is named for the file that holds them.
fn defaults_table() -> BTreeMap<&'static str, Value> {
    BTreeMap::from([
        (
            "chat/startonmention",
            Value::Bool(defaults::START_ON_MENTION),
        ),
        (
            "sandbox/requirefullenforcement",
            Value::Bool(defaults::REQUIRE_FULL_ENFORCEMENT),
        ),
        ("sandbox/network", Value::from("restricted")),
        ("sandbox/egressports", serde_json::json!([443])),
        (
            "sandbox/egress",
            serde_json::json!({
                "mode": "proxy",
                "allow": ["*"],
                "allowInternal": defaults::EGRESS_ALLOW_INTERNAL,
            }),
        ),
        (
            "sandbox/hidehostaddress",
            Value::Bool(defaults::HIDE_HOST_ADDRESS),
        ),
        ("sandbox/image", Value::from(defaults::IMAGE)),
        ("sandbox/memory", Value::from(defaults::MEMORY)),
        ("sandbox/cpus", serde_json::json!(defaults::CPUS)),
        ("sandbox/pids", serde_json::json!(defaults::PIDS)),
        ("sandbox/filemax", Value::from(defaults::FILE_MAX)),
        ("sandbox/disk", Value::from(defaults::DISK)),
        (
            "sandbox/diskcheckms",
            serde_json::json!(defaults::DISK_CHECK_MS),
        ),
        (
            "sandbox/graceperiodms",
            serde_json::json!(defaults::GRACE_PERIOD_MS),
        ),
        (
            "output/forwardtooloutput",
            Value::Bool(defaults::FORWARD_TOOL_OUTPUT),
        ),
        (
            "output/maxtooloutputchars",
            serde_json::json!(defaults::MAX_TOOL_OUTPUT_CHARS),
        ),
        (
            "output/maxattachmentbytes",
            serde_json::json!(defaults::MAX_ATTACHMENT_BYTES),
        ),
        (
            "output/maxattachmentspermessage",
            serde_json::json!(defaults::MAX_ATTACHMENTS_PER_MESSAGE),
        ),
        ("output/postdiffs", Value::Bool(defaults::POST_DIFFS)),
        (
            "limits/maxconcurrentturns",
            serde_json::json!(defaults::MAX_CONCURRENT_TURNS),
        ),
        (
            "limits/maxlivesessions",
            serde_json::json!(defaults::MAX_LIVE_SESSIONS),
        ),
        (
            "limits/maxqueuelength",
            serde_json::json!(defaults::MAX_QUEUE_LENGTH),
        ),
        (
            "limits/maxqueuewaitms",
            serde_json::json!(defaults::MAX_QUEUE_WAIT_MS),
        ),
        ("timeouts/idlems", serde_json::json!(defaults::IDLE_MS)),
        ("web/host", Value::from(defaults::WEB_HOST)),
        ("web/port", serde_json::json!(defaults::WEB_PORT)),
        ("web/observer", Value::Bool(defaults::WEB_OBSERVER)),
    ])
}

/// Which key each section is written under, in the order they are documented.
///
/// Ordered here rather than taken from the file, because what a reader needs
/// first is not the order the types happen to be declared in.
const KEYS: [(&str, &str); 13] = [
    ("Root", "top level"),
    ("Chat", "chat"),
    ("Agent", "agent"),
    ("Delegate", "agent.delegate"),
    ("Github", "github"),
    ("Sandbox", "sandbox"),
    ("Egress", "sandbox.egress"),
    ("PolicyExtra", "sandbox.policyExtra"),
    ("Output", "output"),
    ("Web", "web"),
    ("Shutdown", "shutdown"),
    ("Limits", "limits"),
    ("Timeouts", "timeouts"),
];

/// The configuration reference.
pub fn configuration_page(
    sections: &[Section],
    required: &std::collections::BTreeSet<String>,
) -> String {
    let mut out = String::from(
        "<!-- Generated by the docs module. Edit the schema, not this. -->\n\
         \n\
         # Configuration\n\
         \n\
         Every field the daemon reads, taken from `src/config/schema.ts`. A key that\n\
         is not one of these is a refusal to start rather than a setting quietly\n\
         ignored, so a misspelling says so.\n\
         \n\
         The file is read from `~/.config/errand/config.json`, then\n\
         `/etc/errand/config.json`, then `config.json` in the working directory.\n\
         `ERRAND_CONFIG` names one outright.\n\
         \n\
         There is a file to copy in the repository, `config.example.json`, and a\n\
         schema beside it. Naming the schema in your own file gives an editor\n\
         completion and checking as you type:\n\
         \n\
         ```json\n\
         { \"$schema\": \"https://raw.githubusercontent.com/QaidVoid/errand/main/config.schema.json\" }\n\
         ```\n\
         \n",
    );

    for (name, key) in KEYS {
        let Some(section) = sections.iter().find(|found| found.name == name) else {
            continue;
        };
        // A field whose type is another section is documented as that section.
        let fields: Vec<&Field> = section
            .fields
            .iter()
            .filter(|field| !shown_type(&field.type_).ends_with("Config | undefined"))
            .collect();
        if fields.is_empty() {
            continue;
        }

        let _ = writeln!(out, "## {key}\n");
        if !section.says.is_empty() {
            let _ = writeln!(out, "{}\n", section.says);
        }
        out.push_str("| field | type | default | what it does |\n| --- | --- | --- | --- |\n");
        for field in fields {
            let shown = shown_type(&field.type_).replace('|', "or");
            let fallback = default_of(name, &field.name);
            let stands = if !fallback.is_empty() {
                fallback
            } else if required.contains(&format!("{key}.{}", field.name))
                || required.contains(&field.name)
            {
                "required".to_owned()
            } else {
                "none".to_owned()
            };
            let _ = writeln!(
                out,
                "| `{}` | `{shown}` | {stands} | {} |",
                field.name, field.says
            );
        }
        out.push('\n');
    }

    out
}

/// How a field's type is written for an editor that reads JSON Schema.
fn json_type(type_: &str, sections: &[Section]) -> Value {
    let shown = shown_type(type_);
    let bare = shown.trim_end_matches(" | undefined").trim();
    match bare {
        "string" => json!({ "type": "string" }),
        "number" => json!({ "type": "number" }),
        "boolean" => json!({ "type": "boolean" }),
        "string[]" => json!({ "type": "array", "items": { "type": "string" } }),
        "number[]" => json!({ "type": "array", "items": { "type": "number" } }),
        "Record<string, string>" => json!({
            "type": "object",
            "additionalProperties": { "type": "string" },
        }),
        "SandboxBackend" => json!({ "type": "string", "enum": ["podman", "bailey"] }),
        "NetworkMode" => json!({ "type": "string", "enum": ["restricted", "none"] }),
        _ => {
            let section = sections.iter().find(|section| {
                shown.trim_end_matches(" | undefined").trim() == format!("{}Config", section.name)
            });
            match section {
                Some(section) => object_for(section, sections),
                // Anything this does not recognise is left unconstrained
                // rather than constrained wrongly.
                None => json!({}),
            }
        }
    }
}

/// One section as a JSON Schema object.
fn object_for(section: &Section, sections: &[Section]) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();

    for field in &section.fields {
        let described = json_type(&field.type_, sections);
        let mut described = match described {
            Value::Object(map) => map,
            other => {
                let mut map = Map::new();
                map.insert("type".to_owned(), other);
                map
            }
        };
        described.insert("description".to_owned(), Value::String(field.says.clone()));
        properties.insert(field.name.clone(), Value::Object(described));
        let optional = shown_type(&field.type_).contains("undefined");
        if !optional && default_of(&section.name, &field.name).is_empty() {
            required.push(Value::String(field.name.clone()));
        }
    }

    let mut object = Map::new();
    object.insert("type".to_owned(), Value::from("object"));
    object.insert(
        "description".to_owned(),
        Value::String(section.says.clone()),
    );
    object.insert("additionalProperties".to_owned(), Value::Bool(false));
    object.insert("properties".to_owned(), Value::Object(properties));
    if !required.is_empty() {
        object.insert("required".to_owned(), Value::Array(required));
    }
    Value::Object(object)
}

/// The configuration file, as a schema an editor can check against.
pub fn config_schema(sections: &[Section]) -> String {
    let root = sections
        .iter()
        .find(|section| section.name == "Root")
        .unwrap_or_else(|| panic!("the top-level interface was not found"));

    let mut schema = object_for(root, sections);
    schema["$schema"] = Value::from("https://json-schema.org/draft/2020-12/schema");
    schema["$id"] =
        Value::from("https://raw.githubusercontent.com/QaidVoid/errand/main/config.schema.json");
    schema["title"] = Value::from("errand configuration");
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert(
            "$schema".to_owned(),
            json!({ "type": "string", "description": "Where this schema lives." }),
        );
    }

    format!(
        "{}\n",
        serde_json::to_string_pretty(&schema).unwrap_or_default()
    )
}

/// The command reference.
pub fn commands_page() -> String {
    let access = |access: CommandAccess| match access {
        CommandAccess::Anyone => "anyone permitted",
        CommandAccess::Guest => "the owner and whoever they invited",
        CommandAccess::Owner => "the owner and operators",
        CommandAccess::Host => "named accounts, answered by the daemon",
    };
    let groups: [(CommandGroup, &str); 4] = [
        (CommandGroup::Session, "The session"),
        (CommandGroup::People, "Who takes part"),
        (CommandGroup::Project, "The project"),
        (CommandGroup::You, "You and the host"),
    ];

    let mut out = String::from(
        "<!-- Generated by the docs module. Edit the command table, not this. -->\n\
         \n\
         # Commands\n\
         \n\
         Typed in a thread, or picked as a slash command: both run the same code, so\n\
         what they do and who may do it cannot drift apart.\n\
         \n\
         Anything else is a prompt for the agent. A message starting `!!!` is an\n\
         aside: everyone in the thread sees it and the agent is never told.\n\
         \n",
    );

    for (group, title) in groups {
        let in_group: Vec<_> = COMMANDS
            .iter()
            .filter(|(_, meta)| meta.group == group)
            .collect();
        if in_group.is_empty() {
            continue;
        }

        let _ = writeln!(out, "## {title}\n");
        out.push_str("| command | who may | what it does |\n| --- | --- | --- |\n");
        for (name, meta) in in_group {
            let spelled = meta
                .argument
                .map_or_else(|| name.to_string(), |argument| format!("{name} {argument}"));
            let _ = writeln!(
                out,
                "| `{spelled}` | {} | {} |",
                access(meta.access),
                meta.summary
            );
        }
        out.push('\n');
    }

    out
}

/// The character table, which is the whole set this system may emit.
pub fn characters_page() -> String {
    let glyph_of = |codepoints: &[&str]| {
        codepoints
            .iter()
            .map(|point| format!("`{point}`"))
            .collect::<Vec<_>>()
            .join(" ")
    };

    let mut out = String::from(
        "<!-- Generated by the docs module. Edit the table, not this. -->\n\
         \n\
         # Characters\n\
         \n\
         Every character outside ASCII this system emits, and the one state each\n\
         means. None is decoration, and using one for a state it does not name is a\n\
         bug. Everything else, including every log line, is ASCII.\n\
         \n\
         They are declared as codepoints rather than as glyphs, so the source stays\n\
         ASCII and an editor that cannot render one cannot corrupt it.\n\
         \n\
         ## Reactions\n\
         \n\
         Placed on the sender's own message, tracking that message's fate. Exactly\n\
         one is present at a time.\n\
         \n\
         | codepoints | name | means |\n\
         | --- | --- | --- |\n",
    );

    for reaction in [
        ReactionKey::Accepted,
        ReactionKey::Succeeded,
        ReactionKey::Failed,
        ReactionKey::Interrupted,
    ] {
        let entry = reaction.entry();
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            glyph_of(entry.codepoints),
            entry.name,
            entry.meaning
        );
    }

    out.push_str(
        "\n## Prefixes\n\nPlaced at the start of a line the daemon writes.\n\n\
         | codepoints | name | means |\n| --- | --- | --- |\n",
    );
    for prefix in ALL_PREFIXES {
        let entry = prefix.entry();
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            glyph_of(entry.codepoints),
            entry.name,
            entry.meaning
        );
    }
    let _ = writeln!(out, "\nIn total {} characters.\n", all_chars().len());

    out
}

/// What each generated page should hold, by where it lives, relative to the
/// repository root.
pub fn pages(schema: &str) -> BTreeMap<String, String> {
    let sections = read_schema(schema);
    BTreeMap::from([
        ("config.schema.json".to_owned(), config_schema(&sections)),
        (
            "docs/reference/configuration.md".to_owned(),
            configuration_page(&sections, &required_fields()),
        ),
        ("docs/reference/commands.md".to_owned(), commands_page()),
        ("docs/reference/characters.md".to_owned(), characters_page()),
    ])
}

/// The repository root, which is the crate itself since the cutover.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .expect("the crate root is the repository root")
}

/// Which committed pages no longer match the code, oldest first.
pub fn stale_pages(root: &Path) -> Vec<String> {
    let schema = std::fs::read_to_string(root.join("src/config/schema.rs"))
        .expect("the schema is in the tree");
    pages(&schema)
        .into_iter()
        .filter(|(path, wanted)| {
            std::fs::read_to_string(root.join(path)).is_ok_and(|held| held != *wanted)
        })
        .map(|(path, _)| path)
        .collect()
}

/// Brings the committed pages back in step, naming what changed.
pub fn write_pages(root: &Path) -> Vec<String> {
    let schema = std::fs::read_to_string(root.join("src/config/schema.rs"))
        .expect("the schema is in the tree");
    let mut written = Vec::new();
    for (path, wanted) in pages(&schema) {
        let full = root.join(&path);
        if std::fs::read_to_string(&full).is_ok_and(|held| held == wanted) {
            continue;
        }
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&full, wanted).expect("the page is written");
        written.push(path);
    }
    written
}

#[cfg(test)]
mod tests;
