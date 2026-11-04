//! Writes the reference pages from the code they describe.
//!
//! A configuration reference written by hand is wrong the first time a field
//! changes, and wrong documentation is worse than none: it is believed. The
//! schema, the command table, and the character table all already say what
//! they mean, so the pages are made from them and the check fails when what
//! is committed no longer matches.
//!

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

/// Reads the configuration input sections from the resolved schema.
///
/// A scanner rather than a parser: the file is written to one shape, every
/// field carries a doc comment, and anything it cannot read is a failure
/// rather than a field quietly left out.
pub fn read_schema(source: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut comment: Vec<String> = Vec::new();
    let mut attribute_depth = 0_i32;
    // A field the schema renames for the file's sake, such as `kind` being
    // spelled `type` there. The next field takes the name; anything else
    // lets it go.
    let mut rename: Option<String> = None;

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
            rename = trimmed
                .split("rename = \"")
                .nth(1)
                .and_then(|rest| rest.split('"').next().map(str::to_owned));
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
            // Schema structs are named for their key (`ChatConfig`),
            // and the pages follow the key rather than the whole name. The
            // bare `Config` is the root, documented as the top level. A struct
            // named for nothing else, such as `ForgeTrigger`, is a sub-section
            // documented under whichever key names it.
            let says = std::mem::take(&mut comment).join(" ");
            let key = match name.strip_suffix("Config") {
                Some("") => "Root",
                Some(key) => key,
                None => name.as_str(),
            };
            rename = None;
            sections.push(Section {
                name: key.to_owned(),
                says,
                fields: Vec::new(),
            });
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
            let name = rename.take().unwrap_or_else(|| to_camel(name.trim()));
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

    project_forge_input(&mut sections);

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

/// Projects the forge's resolved fields back to the keys accepted on disk.
fn project_forge_input(sections: &mut Vec<Section>) {
    if let Some(trigger) = sections
        .iter_mut()
        .find(|section| section.name == "ForgeTrigger")
    {
        trigger
            .fields
            .retain(|field| !matches!(field.name.as_str(), "onMention" | "onAssign"));
        trigger.fields.push(Field {
            name: "on".to_owned(),
            type_: "Option<Vec<String>>".to_owned(),
            says: "Events that start a session: `mention`, `assign`, or both. Must not be empty. Omitted means both events.".to_owned(),
        });
        if let Some(allowed) = trigger
            .fields
            .iter_mut()
            .find(|field| field.name == "allowedUsers")
        {
            allowed
                .says
                .push_str(" Required and non-empty. A wildcard `*` is refused.");
        }
    }
    let Some(forge) = sections.iter().find(|section| section.name == "Forge") else {
        return;
    };
    let legacy = Section {
        name: "Github".to_owned(),
        says: "Deprecated but still supported: the legacy GitHub identity. Rename `github` to `forge`; `forge.type` defaults to `github`. Do not set both sections. Only `token`, `userName`, `userEmail`, `trigger`, and `app` are accepted here, not `type` or `url`. The optional `trigger` and `app` objects use the fields documented under `forge.trigger` and `forge.app`.".to_owned(),
        fields: forge.fields.iter()
            .filter(|field| !matches!(field.name.as_str(), "type" | "url"))
            .cloned()
            .collect(),
    };
    sections.push(legacy);
    if let Some(root) = sections.iter_mut().find(|section| section.name == "Root") {
        root.fields.push(Field {
            name: "github".to_owned(),
            type_: "Option<Github>".to_owned(),
            says: "Deprecated GitHub configuration. Use `forge` instead; setting both is refused."
                .to_owned(),
        });
    }
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
    let trimmed = type_.trim();
    // One `Option<...>` wrapper comes off whole, so a generic inside one
    // keeps its own closing bracket.
    let (bare, optional) = match trimmed.strip_prefix("Option<") {
        Some(inner) => match inner.strip_suffix('>') {
            Some(inner) => (inner.trim(), true),
            None => (trimmed, false),
        },
        None => (trimmed, false),
    };
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
                let at = problem
                    .find(" is required")
                    .or_else(|| problem.find(" must be the number GitHub shows for it"))
                    .unwrap_or(problem.len());
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
    collect(&serde_json::json!({ "forge": {}, "agent": { "delegate": {} } }));
    collect(&serde_json::json!({ "github": {} }));
    collect(&serde_json::json!({ "forge": { "app": {} } }));
    collect(&serde_json::json!({ "github": { "app": {} } }));
    collect(&serde_json::json!({ "web": { "login": {} } }));
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
#[expect(clippy::too_many_lines, reason = "one row per documented default")]
fn defaults_table() -> BTreeMap<&'static str, Value> {
    let egress = serde_json::json!({
        "mode": defaults::EGRESS_MODE,
        "allow": defaults::EGRESS_ALLOW,
        "allowInternal": defaults::EGRESS_ALLOW_INTERNAL,
    });
    let incus = serde_json::json!({
        "project": defaults::INCUS_PROJECT,
        "image": defaults::INCUS_IMAGE,
        "rootSize": defaults::INCUS_ROOT_SIZE,
        "guestUid": defaults::INCUS_GUEST_UID,
    });
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
        (
            "sandbox/egressports",
            serde_json::json!(defaults::EGRESS_PORTS),
        ),
        ("sandbox/egress", egress.clone()),
        // The sub-section's own rows are looked up as `egress/<field>`, with
        // the whole key lower-cased.
        ("egress/mode", egress["mode"].clone()),
        // A block that leaves `allow` out names no host. Only the whole block
        // left out means the default list above.
        ("egress/allow", serde_json::json!([])),
        ("egress/allowinternal", egress["allowInternal"].clone()),
        ("sandbox/incus", incus.clone()),
        ("incus/project", incus["project"].clone()),
        ("incus/image", incus["image"].clone()),
        ("incus/rootsize", incus["rootSize"].clone()),
        ("incus/guestuid", incus["guestUid"].clone()),
        (
            "retention/enabled",
            Value::Bool(defaults::RETENTION_ENABLED),
        ),
        (
            "retention/olderthan",
            Value::from(defaults::RETENTION_OLDER_THAN),
        ),
        ("retention/every", Value::from(defaults::RETENTION_EVERY)),
        // Left out, a token asks for everything its installation was granted.
        ("forgeapp/permissions", serde_json::json!({})),
        ("forgeapp/repositories", serde_json::json!([])),
        ("forge/type", Value::from("github")),
        // Left out, a trigger hears both a mention and an assignment, and
        // wherever it is mentioned or assigned.
        ("forgetrigger/on", serde_json::json!(["mention", "assign"])),
        ("forgetrigger/repositories", serde_json::json!([])),
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
        (
            "output/maxuploadbytes",
            serde_json::json!(defaults::MAX_UPLOAD_BYTES),
        ),
        ("output/postdiffs", Value::Bool(defaults::POST_DIFFS)),
        ("output/postimages", Value::Bool(defaults::POST_IMAGES)),
        ("output/postaudio", Value::Bool(defaults::POST_AUDIO)),
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
const KEYS: [(&str, &str); 19] = [
    ("Root", "top level"),
    ("Chat", "chat"),
    ("Agent", "agent"),
    ("Delegate", "agent.delegate"),
    ("Forge", "forge"),
    ("ForgeTrigger", "forge.trigger"),
    ("ForgeApp", "forge.app"),
    ("Github", "github"),
    ("Sandbox", "sandbox"),
    ("Egress", "sandbox.egress"),
    ("Incus", "sandbox.incus"),
    ("PolicyExtra", "sandbox.policyExtra"),
    ("Output", "output"),
    ("Web", "web"),
    ("WebLogin", "web.login"),
    ("Shutdown", "shutdown"),
    ("Limits", "limits"),
    ("Timeouts", "timeouts"),
    ("Retention", "retention"),
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
         Every field the daemon accepts, projected from `src/config/schema.rs`. A key that\n\
         is not one of these is a refusal to start rather than a setting quietly\n\
         ignored, so a misspelling says so. Inside a provider definition, which may\n\
         carry keys of the operator's own, only a misspelling of a route key is\n\
         refused, and anything else is ignored.\n\
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
        // A field whose type is another section is documented as that
        // section, under the section's own key.
        let fields: Vec<&Field> = section
            .fields
            .iter()
            .filter(|field| {
                let bare = shown_type(&field.type_)
                    .trim_end_matches(" | undefined")
                    .trim()
                    .to_owned();
                !sections
                    .iter()
                    .any(|other| bare == other.name || bare == format!("{}Config", other.name))
            })
            .collect();
        if fields.is_empty() {
            continue;
        }

        let _ = writeln!(out, "## {key}\n");
        if !section.says.is_empty() {
            let _ = writeln!(out, "{}\n", section.says);
        }
        if name == "Forge" {
            out.push_str(
                "For Forgejo, set `type` to `forgejo` and `url` to the instance address. \
                 Use HTTPS: HTTP sends the forge token and repository traffic in cleartext, \
                 exposing them to anyone who can observe or alter the connection.\n\n\
                 Forgejo assignment triggers only cover direct assignments to the bot, not team \
                 assignments. Its notification floor is kept in memory only. On restart, unread \
                 activity from before the restart is skipped, including activity from failed reads.\n\n\
                 ```json\n\
                 {\n\
                 \x20 \"forge\": {\n\
                 \x20   \"type\": \"forgejo\",\n\
                 \x20   \"url\": \"https://forge.example.com\",\n\
                 \x20   \"token\": \"PUT A FORGE TOKEN HERE\",\n\
                 \x20   \"userName\": \"Commit Author\",\n\
                 \x20   \"userEmail\": \"author@example.com\"\n\
                 \x20 }\n\
                 }\n\
                 ```\n\n",
            );
        }
        out.push_str("| field | type | default | what it does |\n| --- | --- | --- | --- |\n");
        for field in fields {
            let shown = shown_type(&field.type_).replace('|', "or");
            let fallback = default_of(name, &field.name);
            let stands = if !fallback.is_empty() {
                fallback
            } else if (name == "ForgeTrigger" && field.name == "allowedUsers")
                || required.contains(&format!("{key}.{}", field.name))
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
        "SandboxBackend" => json!({ "type": "string", "enum": ["podman", "bailey", "incus"] }),
        "NetworkMode" => json!({ "type": "string", "enum": ["restricted", "none"] }),
        "ForgeKind" => json!({ "type": "string", "enum": ["github", "forgejo"] }),
        _ => {
            let section = sections.iter().find(|section| {
                let bare = shown.trim_end_matches(" | undefined").trim();
                bare == format!("{}Config", section.name)
                    || (bare == section.name && section.name != "Root")
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
        match (section.name.as_str(), field.name.as_str()) {
            ("Forge", "type") => {
                described.insert("default".to_owned(), json!("github"));
            }
            ("ForgeTrigger", "on") => {
                described.insert(
                    "items".to_owned(),
                    json!({
                        "type": "string", "enum": ["mention", "assign"],
                    }),
                );
                described.insert("minItems".to_owned(), json!(1));
                described.insert("default".to_owned(), json!(["mention", "assign"]));
            }
            ("ForgeTrigger", "allowedUsers") => {
                described.insert("minItems".to_owned(), json!(1));
            }
            ("ForgeTrigger", "repositories") => {
                described.insert("default".to_owned(), json!([]));
            }
            _ => {}
        }
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
    if section.name == "Github" {
        object.insert("deprecated".to_owned(), Value::Bool(true));
    }
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
    schema["not"] = json!({ "required": ["forge", "github"] });
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
    let groups = [
        CommandGroup::Session,
        CommandGroup::People,
        CommandGroup::Project,
        CommandGroup::You,
    ]
    .map(|group| (group, group.title()));

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
