//! Compiles the renderer's shaders to SPIR-V at build time.
//!
//! naga rather than shaderc or glslc: it is pure Rust, so building
//! OpenVideoServer needs no C++ toolchain and no external shader compiler on the
//! build machine. Compiling here rather than at startup makes a broken shader a
//! build failure instead of a renderer that comes up and shows nothing.
//!
//! The shaders are WGSL, which is not a preference. naga's GLSL front end does
//! not implement binding qualifiers, so a GLSL shader cannot name a texture or a
//! sampler, which rules it out for everything past a flat colour.

use std::path::Path;

use naga::back::spv;
use naga::front::wgsl;
use naga::valid::{Capabilities, ValidationFlags, Validator};

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let out_dir = Path::new(&out_dir);

    for (source, output) in [
        ("shaders/canvas.wgsl", "canvas.spv"),
        ("shaders/effect.wgsl", "effect.spv"),
        ("shaders/generator.wgsl", "generator.spv"),
    ] {
        println!("cargo:rerun-if-changed={source}");
        let text = std::fs::read_to_string(source)
            .unwrap_or_else(|error| panic!("cannot read {source}: {error}"));
        let words = compile(&text, source);

        let mut bytes = Vec::with_capacity(words.len() * 4);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        std::fs::write(out_dir.join(output), &bytes)
            .unwrap_or_else(|error| panic!("cannot write {output}: {error}"));
    }
}

/// Compiles one WGSL module, keeping every entry point in it.
///
/// One module with both entry points rather than one per stage: Vulkan selects
/// the stage by entry point name, so splitting them would mean loading the same
/// module twice.
fn compile(source: &str, name: &str) -> Vec<u32> {
    let module = wgsl::parse_str(source).unwrap_or_else(|error| {
        panic!("{name} does not compile: {}", error.emit_to_string(source))
    });

    // Immediates — push constants, as Vulkan calls them and as naga used to.
    // They are how an effect pass is told which effect it is and what it was
    // set to. The alternative is a uniform buffer per pass, which is an
    // allocation and a descriptor write in the render path for forty-eight
    // bytes. Every Vulkan implementation offers at least 128.
    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::IMMEDIATES);
    let info = validator
        .validate(&module)
        .unwrap_or_else(|error| panic!("{name} is not valid: {error:?}"));

    spv::write_vec(&module, &info, &spv::Options::default(), None)
        .unwrap_or_else(|error| panic!("{name} cannot be lowered to SPIR-V: {error:?}"))
}
