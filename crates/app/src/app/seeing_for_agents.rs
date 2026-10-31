//! What agents see of the windows: which there are, and pictures of them,
//! or of a panel, as they're drawn. egui takes a window's picture as it
//! paints it, so a covered window, or one on another monitor, comes out
//! whole.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use eframe::egui::{self, ColorImage, Rect, ScreenshotCallback, ViewportCommand};
use musicable_mcp::{Reply, Shot};
use serde_json::{Value as Json, json};

use super::App;

impl App {
    /// The main window and the windows of their own that panels moved to,
    /// with where each is, and the dialogs open over the main window.
    pub(super) fn windows_for_agent(&self, ctx: &egui::Context) -> Json {
        let name = self.name();
        let window = |window: Option<usize>, title: String| {
            let viewport = self.shell.window_viewport(window)?;
            let (place, minimized, focused) = ctx.input_for(viewport, |input| {
                let info = input.viewport();
                let scale = info.native_pixels_per_point.unwrap_or(1.0);
                let place = info.outer_rect.map(|rect| {
                    let at = rect.min * scale;
                    let size = rect.size() * scale;
                    json!({
                        "at": [at.x.round() as i32, at.y.round() as i32],
                        "size": [size.x.round() as u32, size.y.round() as u32],
                    })
                });
                (place, info.minimized, info.focused)
            });
            let panels: Vec<&str> = (self.shell.layout().panels.into_iter())
                .filter(|panel| panel.window == window.map(|index| index + 1) && panel.in_sight)
                .map(|panel| panel.id)
                .collect();
            let mut entry = json!({
                "window": window.map_or(json!("main"), |index| json!(index + 1)),
                "title": title,
                "minimized": minimized.unwrap_or(false),
                "focused": focused.unwrap_or(false),
                "panels": panels,
            });
            if let Some(Json::Object(place)) = place {
                entry.as_object_mut()?.extend(place);
            }
            Some(entry)
        };
        let main = window(None, format!("{name} - Musicable"));
        let own = (0..self.shell.windows())
            .filter_map(|index| window(Some(index), self.shell.window_title(index, &name)?));
        let mut dialogs = Vec::new();
        if self.settings_window.is_open() {
            dialogs.push("Settings");
        }
        if self.export_window.is_open() {
            dialogs.push("Export Audio");
        }
        if self.pending.is_some() {
            dialogs.push("Save Changes");
        }
        json!({
            "windows": main.into_iter().chain(own).collect::<Vec<_>>(),
            "dialogs": dialogs,
        })
    }

    /// Takes a picture of the window or the panel `shot` names, as it's
    /// next drawn, and answers `reply` with it, or says why it can't.
    pub(super) fn screenshot_for_agent(&mut self, shot: Shot, reply: Reply, ctx: &egui::Context) {
        let (window, crop) = match &shot.panel {
            Some(panel) => {
                if !self
                    .shell
                    .layout()
                    .panels
                    .iter()
                    .any(|place| place.id == panel)
                {
                    return reply.failed(format!(
                        "There's no panel {panel}: get_layout gives their ids."
                    ));
                }
                let Some((window, rect)) = self.shell.panel_rect(panel) else {
                    return reply.failed(format!(
                        "The panel {panel} isn't in sight: view.toggle_{panel} shows it, if it \
                         has a command of its own, or its tab brings it forward."
                    ));
                };
                (window, Some(rect))
            }
            None => (shot.window, None),
        };
        let Some(viewport) = self.shell.window_viewport(window) else {
            let windows = self.shell.windows();
            return reply.failed(format!(
                "There's no window {}: there are {windows} windows of their own besides the main \
                 one.",
                window.map_or(0, |index| index + 1)
            ));
        };
        // eframe paints the windows of their own as egui draws them, and
        // takes no pictures of those.
        if window.is_some() {
            return reply.failed(
                "Musicable can't take pictures of the windows panels moved to yet, only of the \
                 main window and the panels in it. view.move_to_main_window brings the panel \
                 with the focus back.",
            );
        }
        let minimized = ctx.input_for(viewport, |input| input.viewport().minimized);
        if minimized == Some(true) {
            return reply.failed("The window is minimized, so it isn't drawn.");
        }
        let scale = ctx.input_for(viewport, |input| input.pixels_per_point());
        let name = json!({
            "window": window.map_or(json!("main"), |index| json!(index + 1)),
            "panel": shot.panel,
        });
        let file = shot.file;
        let taken = ScreenshotCallback::new(move |image: Arc<ColorImage>| {
            // Off the window's thread: encoding a big window takes a while.
            std::thread::spawn(move || {
                let image = match crop {
                    Some(rect) => cropped(&image, rect, scale),
                    None => (*image).clone(),
                };
                match picture(&image, name, file.as_deref()) {
                    Ok(answer) => reply.done(answer),
                    Err(why) => reply.failed(why),
                }
            });
        });
        ctx.send_viewport_cmd_to(viewport, ViewportCommand::Screenshot(taken));
        ctx.request_repaint_of(viewport);
    }
}

/// The part of `image` in `rect`, in points, at `scale` pixels a point.
fn cropped(image: &ColorImage, rect: Rect, scale: f32) -> ColorImage {
    let [width, height] = image.size;
    let pixel = |value: f32, most: usize| ((value * scale).round().max(0.0) as usize).min(most);
    let (left, right) = (pixel(rect.min.x, width), pixel(rect.max.x, width));
    let (top, bottom) = (pixel(rect.min.y, height), pixel(rect.max.y, height));
    let pixels = (top..bottom)
        .flat_map(|y| {
            image.pixels[y * width + left..y * width + right]
                .iter()
                .copied()
        })
        .collect();
    ColorImage::new([right - left, bottom - top], pixels)
}

/// The answer for a picture of `image` that `name` names: its size, the
/// file it's saved to if there's one, and the PNG file in base64.
fn picture(image: &ColorImage, mut name: Json, file: Option<&Path>) -> Result<Json, String> {
    let png = png(image).map_err(|error| format!("The picture couldn't be made: {error}."))?;
    if let Some(file) = file {
        std::fs::write(file, &png)
            .map_err(|error| format!("Couldn't save {}: {error}.", file.display()))?;
    }
    name["size"] = json!(image.size);
    name["file"] = json!(file);
    name["png"] = base64::engine::general_purpose::STANDARD
        .encode(&png)
        .into();
    Ok(name)
}

/// `image` as a PNG file.
fn png(image: &ColorImage) -> Result<Vec<u8>, png::EncodingError> {
    let mut file = Vec::new();
    let [width, height] = image.size;
    let mut encoder = png::Encoder::new(&mut file, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let pixels: Vec<u8> = (image.pixels.iter())
        .flat_map(|pixel| pixel.to_srgba_unmultiplied())
        .collect();
    writer.write_image_data(&pixels)?;
    writer.finish()?;
    file.flush().ok();
    Ok(file)
}

#[cfg(test)]
mod tests {
    use egui::Color32;

    use super::*;

    #[test]
    fn a_panel_is_cut_out_of_its_window() {
        let mut image = ColorImage::filled([8, 6], Color32::BLACK);
        image.pixels[3 * 8 + 4] = Color32::RED;
        let rect = Rect::from_min_max(egui::pos2(1.0, 1.0), egui::pos2(3.0, 2.5));
        let panel = cropped(&image, rect, 2.0);
        assert_eq!(panel.size, [4, 3]);
        assert_eq!(panel.pixels[4 + 2], Color32::RED);
        // Past the window's edge, it stops at the edge.
        let past = Rect::from_min_max(egui::pos2(2.0, 0.0), egui::pos2(10.0, 10.0));
        assert_eq!(cropped(&image, past, 1.0).size, [6, 6]);
    }

    #[test]
    fn pictures_are_png_files() {
        let image = ColorImage::filled([3, 2], Color32::from_rgb(10, 20, 30));
        let answer = picture(&image, json!({"window": "main"}), None).unwrap();
        assert_eq!(answer["size"], json!([3, 2]));
        assert_eq!(answer["file"], Json::Null);
        let png = base64::engine::general_purpose::STANDARD
            .decode(answer["png"].as_str().unwrap())
            .unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut pixels).unwrap();
        assert_eq!(&pixels[..4], &[10, 20, 30, 255]);
    }
}
