use musicable_commands::Registry;
use musicable_model::Project;

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

fn main() {
    let project = Project::new();
    let mut commands = Registry::new();
    commands.register("transport.play", "Play");
    commands.register("transport.stop", "Stop");
    println!(
        "musicable {}: {} tracks, {} commands registered",
        env!("CARGO_PKG_VERSION"),
        project.tracks.len(),
        commands.iter().count()
    );
}
