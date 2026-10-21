use musicable_commands::Registry;
use musicable_model::Project;

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
