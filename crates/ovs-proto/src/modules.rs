//! The generator and effect catalogues, as a document somebody can read.
//!
//! `reference/FIXTURES.md` exists because a lighting programmer patches a console
//! from a printed channel list and a list that disagrees with the machine is
//! worse than none. The same is true of these: an operator choosing a generator
//! or an effect wants to know what it takes before they open the client, and a
//! list maintained by hand drifts the first time somebody publishes a new
//! version.
//!
//! So it is generated from the manifests, committed, and checked by a golden
//! test — the same arrangement, for the same reason.

use ovs_types::config::effects::PUBLISHED as EFFECTS;
use ovs_types::config::generators::PUBLISHED as GENERATORS;
use ovs_types::config::manifest::{ParameterDefinition, ParameterKind};

/// Renders the catalogues as the document an operator reads.
#[must_use]
pub fn markdown() -> String {
    let mut out = String::new();

    out.push_str("# OpenVideoServer — Generators and Effects\n\n");
    out.push_str(
        "Generated from the published manifests by `cargo run -p ovs-proto --bin ovs-module-doc`.\n\
         Do not edit by hand: a printed list that disagrees with the machine is worse than none,\n\
         because it is believed.\n\n\
         A manifest is published once and never edited. Adding a parameter is a new version, so a\n\
         show saved last year opens looking the way it was left.\n\n\
         Every published version is listed here, which is not what a slot offers: a source list\n\
         shows the newest of each type, and an older one is kept so that a show naming it still\n\
         opens. Two entries with the same name are that, rather than a mistake.\n\n",
    );

    out.push_str("## Generators\n\n");
    out.push_str(
        "A generator makes a picture without a file. It implements the same source contract a clip\n\
         does, so a layer plays one exactly as it plays a clip and nothing above it knows the\n\
         difference.\n\n",
    );
    for manifest in &GENERATORS {
        module(
            &mut out,
            manifest.name,
            manifest.type_id,
            manifest.version,
            manifest.description,
            manifest.parameters,
            ovs_types::config::generators::RETIRED.contains(&manifest.type_id),
        );
    }

    out.push_str("## Effects\n\n");
    out.push_str(
        "An effect takes a picture and gives one back, and what comes out is drawn where what\n\
         went in would have been.\n\n\
         **A chain on a layer is what this build draws.** The model also holds a chain on a slot,\n\
         meant to run on the source before the layer's transform; nothing draws that yet, so an\n\
         effect put there is saved and does nothing.\n\n\
         Every effect is neutral at its published defaults: adding one to a chain and touching\n\
         nothing cannot alter a show that was already right.\n\n",
    );
    for manifest in &EFFECTS {
        module(
            &mut out,
            manifest.name,
            manifest.type_id,
            manifest.version,
            manifest.description,
            manifest.parameters,
            ovs_types::config::effects::RETIRED.contains(&manifest.type_id),
        );
    }

    out
}

/// One module's section.
fn module(
    out: &mut String,
    name: &str,
    type_id: &str,
    version: u32,
    description: &str,
    parameters: &[ParameterDefinition],
    retired: bool,
) {
    use std::fmt::Write as _;

    let _ = writeln!(out, "### {name} — `{type_id}` v{version}\n");
    let _ = writeln!(out, "{description}\n");
    if retired {
        out.push_str(
            "**Retired.** Not offered when adding one, and still served to a show that already\n             names it, so nothing that was saved with it loses a layer.\n\n",
        );
    }

    if parameters.is_empty() {
        out.push_str("Takes no parameters.\n\n");
        return;
    }

    out.push_str("| Parameter | Key | Takes | Default |\n|---|---|---|---|\n");
    for parameter in parameters {
        let (takes, default) = match parameter.kind {
            ParameterKind::Number { min, max, default } => (
                format!("a number from {min} to {max}"),
                format!("{default}"),
            ),
            ParameterKind::Colour { default } => (
                "a colour, linear RGBA".to_owned(),
                format!(
                    "{:.2}, {:.2}, {:.2}, {:.2}",
                    default[0], default[1], default[2], default[3]
                ),
            ),
            ParameterKind::Toggle { default } => (
                "on or off".to_owned(),
                if default { "on" } else { "off" }.to_owned(),
            ),
            ParameterKind::Text { default, max_chars } => (
                format!("text, up to {max_chars} characters"),
                if default.is_empty() {
                    "nothing".to_owned()
                } else {
                    format!("`{default}`")
                },
            ),
            ParameterKind::Choice { choices, default } => (
                format!(
                    "one of {}",
                    choices
                        .iter()
                        .map(|choice| format!("`{}` ({})", choice.id, choice.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                format!("`{default}`"),
            ),
        };
        let _ = writeln!(
            out,
            "| {} | `{}` | {takes} | {default} |",
            parameter.name, parameter.key
        );
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_published_module_appears_in_the_document() {
        // The whole point: a module somebody can choose in the client and
        // cannot find in the list is a module nobody knows the parameters of.
        let document = markdown();

        for manifest in &GENERATORS {
            assert!(
                document.contains(manifest.type_id),
                "{} is published and not in the document",
                manifest.type_id
            );
        }
        for manifest in &EFFECTS {
            assert!(
                document.contains(manifest.type_id),
                "{} is published and not in the document",
                manifest.type_id
            );
        }
    }

    #[test]
    fn a_parameter_is_written_with_what_it_takes_and_where_it_starts() {
        // A list without the range and the default is a list somebody has to
        // read the source to use.
        let document = markdown();

        assert!(document.contains("`radius_px`"), "{document}");
        assert!(
            document.contains("a number from 0 to 64"),
            "the blur's radius says its range"
        );
        assert!(
            document.contains("a colour, linear RGBA"),
            "and a colour says what it is"
        );
        assert!(document.contains("on or off"), "and a switch says so");
    }

    #[test]
    fn a_version_is_written_beside_every_module() {
        // A show stores a type and a version, so a document that named only
        // the type would be ambiguous the first time one is published twice.
        let document = markdown();
        for manifest in &EFFECTS {
            assert!(
                document.contains(&format!("`{}` v{}", manifest.type_id, manifest.version)),
                "{} v{} is not written with its version",
                manifest.type_id,
                manifest.version
            );
        }
    }
}
