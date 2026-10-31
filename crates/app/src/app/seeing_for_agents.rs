//! What agents see of the windows: which there are, and pictures of them,
//! or of a panel, as they're drawn. egui takes the main window's picture as
//! it paints it, and Windows the rest's, so a covered window, or one on
//! another monitor, comes out whole.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use eframe::egui::{self, ColorImage, Rect, ScreenshotCallback, ViewportCommand, ViewportId};
use musicable_mcp::{Reply, Shot, Tree};
use musicable_model::DeviceId;
use serde_json::{Value as Json, json};

use super::App;

impl App {
    /// The main window and the windows of their own that panels moved to,
    /// with where each is, and the dialogs open over the main window.
    pub(super) fn windows_for_agent(&self, ctx: &egui::Context) -> Json {
        let name = self.name();
        let zoom = ctx.zoom_factor();
        let window = |window: Option<usize>, title: String| {
            let viewport = self.shell.window_viewport(window)?;
            let (place, minimized, focused) = ctx.input_for(viewport, |input| {
                let info = input.viewport();
                // Points are pixels at the monitor's scaling times egui's
                // zoom.
                let scale = info.native_pixels_per_point.unwrap_or(1.0) * zoom;
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
        let plugins: Vec<Json> = (self.plugin_windows.open_windows(&self.session).into_iter())
            .map(|(device, title, hwnd)| {
                json!({
                    "device": device.0,
                    "title": title,
                    "can_screenshot": cfg!(windows) && hwnd.is_some(),
                })
            })
            .collect();
        json!({
            "windows": main.into_iter().chain(own).collect::<Vec<_>>(),
            "dialogs": dialogs,
            "plugins": plugins,
        })
    }

    /// Takes a picture of the window or the panel `shot` names, as it's
    /// next drawn, and answers `reply` with it, or says why it can't.
    pub(super) fn screenshot_for_agent(&mut self, shot: Shot, reply: Reply, ctx: &egui::Context) {
        if let Some(device) = shot.device {
            return self.plugin_screenshot(device, shot.file, reply);
        }
        let (window, viewport, crop) = match self.place(shot.window, shot.panel.as_deref()) {
            Ok(place) => place,
            Err(why) => return reply.failed(why),
        };
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
        // eframe paints the windows of their own as egui draws them, and
        // takes no pictures of those, so Windows takes them.
        if window.is_some() {
            let Some(hwnd) = super::window_of(ctx, viewport) else {
                return reply.failed(if cfg!(windows) {
                    "Windows didn't say which window that is: try again once it's shown."
                } else {
                    "Musicable takes pictures of the windows panels moved to only on Windows so \
                     far, and of the main window and the panels in it everywhere. \
                     view.move_to_main_window brings the panel with the focus back."
                });
            };
            return captured(hwnd, crop.map(|rect| (rect, scale)), name, file, reply);
        }
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

impl App {
    /// Reads what `tree` asks of a window, as a screen reader does, as it's
    /// next drawn, and answers `reply` with it, or says why it can't.
    pub(super) fn ui_tree_for_agent(&mut self, tree: Tree, reply: Reply, ctx: &egui::Context) {
        let (window, viewport, panel) = match self.place(tree.window, tree.panel.as_deref()) {
            Ok(place) => place,
            Err(why) => return reply.failed(why),
        };
        let minimized = ctx.input_for(viewport, |input| input.viewport().minimized);
        if minimized == Some(true) {
            return reply.failed("The window is minimized, so it isn't drawn.");
        }
        let ask = crate::ui_tree::Ask {
            window: window.map_or(json!("main"), |index| json!(index + 1)),
            panel: tree.panel.zip(panel),
            filter: tree.filter,
            limit: tree.limit,
        };
        crate::ui_tree::read(ctx, viewport, ask, reply);
    }

    /// The window that `window` or `panel` names, as the window, counted
    /// from 0 with `None` for the main one, and its viewport, and where the
    /// panel is in it, or why there isn't one.
    fn place(
        &self,
        window: Option<usize>,
        panel: Option<&str>,
    ) -> Result<(Option<usize>, ViewportId, Option<Rect>), String> {
        let (window, rect) = match panel {
            Some(panel) => {
                if !(self.shell.layout().panels.iter()).any(|place| place.id == panel) {
                    return Err(format!(
                        "There's no panel {panel}: get_layout gives their ids."
                    ));
                }
                let Some((window, rect)) = self.shell.panel_rect(panel) else {
                    return Err(format!(
                        "The panel {panel} isn't in sight: view.toggle_{panel} shows it, if it \
                         has a command of its own, or its tab brings it forward."
                    ));
                };
                (window, Some(rect))
            }
            None => (window, None),
        };
        let Some(viewport) = self.shell.window_viewport(window) else {
            let windows = self.shell.windows();
            return Err(format!(
                "There's no window {}: there are {windows} windows of their own besides the main \
                 one.",
                window.map_or(0, |index| index + 1)
            ));
        };
        Ok((window, viewport, rect))
    }

    /// Takes a picture of the device `device`'s plugin window, and answers
    /// `reply` with it, saved to `file` too if it's given, or says why it
    /// can't.
    fn plugin_screenshot(&self, device: DeviceId, file: Option<PathBuf>, reply: Reply) {
        let open = self.plugin_windows.open_windows(&self.session);
        let Some((_, title, hwnd)) = open.into_iter().find(|(open, ..)| *open == device) else {
            return reply.failed(format!(
                "Device {}'s plugin window isn't open: device.show_window opens it, and \
                 get_windows lists the ones that are.",
                device.0
            ));
        };
        let Some(hwnd) = hwnd.filter(|_| cfg!(windows)) else {
            return reply.failed(if cfg!(windows) {
                "The plugin opened a window of its own, which Musicable can't take pictures of."
            } else {
                "Musicable takes pictures of plugins' windows only on Windows so far."
            });
        };
        let name = json!({"device": device.0, "title": title});
        captured(hwnd, None, name, file, reply);
    }
}

/// Has Windows take a picture of the window `hwnd`, Win32's `HWND`, or of
/// the part of it `crop` gives, in points at so many pixels a point, off
/// the window's thread, and answers `reply` with it as [`picture`] does.
fn captured(
    hwnd: isize,
    crop: Option<(Rect, f32)>,
    name: Json,
    file: Option<PathBuf>,
    reply: Reply,
) {
    std::thread::spawn(move || {
        let answer = crate::window_capture::capture(hwnd).and_then(|image| {
            let image = match crop {
                Some((rect, scale)) => cropped(&image, rect, scale),
                None => image,
            };
            picture(&image, name, file.as_deref())
        });
        match answer {
            Ok(answer) => reply.done(answer),
            Err(why) => reply.failed(why),
        }
    });
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
