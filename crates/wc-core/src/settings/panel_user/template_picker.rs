//! Template-library `ComboBox` widget: thumbnail cache, row rendering, and
//! the two-step delete confirm.
//!
//! Feature-gated on `templates` (this whole module is declared behind
//! `#[cfg(feature = "templates")]` in [`super`]). [`template_library_rows`]
//! snapshots the on-disk library into display rows once per frame (lazily
//! uploading thumbnails); [`render_template_library`] is the `ComboBox`
//! widget itself, called from [`super::widgets::render_widget_value`] for
//! the `TemplateLibrary` setting kind.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bevy::prelude::World;
use bevy_egui::egui;
use egui_phosphor::regular as phosphor;

use crate::templates::resource::{TemplateLibrary, TemplateThumbnailCache};
use crate::templates::view::TemplateRow;
use crate::templates::{store, templates_dir};
use crate::ui::OverlayStyle;

/// Snapshot the template library into display rows, lazily decoding each
/// thumbnail into a session-cached egui texture and attaching its id. Built
/// after the egui ctx clone (it needs `ctx` to upload textures) but before the
/// dock closure borrows `world`. Empty when the resource is absent.
pub(super) fn template_library_rows(world: &mut World, ctx: &egui::Context) -> Vec<TemplateRow> {
    // Pull the rows + (hash, thumb-path) list out so the immutable library
    // borrow is released before we mutate the thumbnail cache below.
    let (mut rows, entries) = {
        let Some(lib) = world.get_resource::<TemplateLibrary>() else {
            return Vec::new();
        };
        let rows = crate::templates::view::build_rows(lib);
        let entries: Vec<(String, PathBuf)> = lib
            .entries
            .iter()
            .map(|e| (e.hash.clone(), lib.dir.join(&e.thumb)))
            .collect();
        (rows, entries)
    };

    // `TemplateThumbnailCache` is registered alongside `TemplateLibrary` by
    // `TemplatesPlugin`, so it is present whenever the library above resolved —
    // the hard `resource()`/`resource_mut()` accesses below cannot panic.
    // Decode + upload any thumbnail not already cached (one-time per session).
    let needed: Vec<(String, PathBuf)> = {
        let cache = world.resource::<TemplateThumbnailCache>();
        entries
            .iter()
            .filter(|(hash, _)| !cache.0.contains_key(hash))
            .cloned()
            .collect()
    };
    for (hash, thumb_path) in needed {
        if let Some(handle) = load_thumb_texture(ctx, &hash, &thumb_path) {
            world
                .resource_mut::<TemplateThumbnailCache>()
                .0
                .insert(hash, handle);
        }
    }

    // Drop cached textures whose template was deleted (frees the GPU texture).
    let live: HashSet<String> = entries.iter().map(|(hash, _)| hash.clone()).collect();
    world
        .resource_mut::<TemplateThumbnailCache>()
        .0
        .retain(|hash, _| live.contains(hash));

    // Attach texture ids to the rows.
    let cache = world.resource::<TemplateThumbnailCache>();
    for row in &mut rows {
        row.thumb = cache.0.get(&row.hash).map(egui::TextureHandle::id);
    }
    rows
}

/// Decode a baked thumbnail PNG and upload it as a session-lived egui texture.
/// `None` on any read/decode failure (the row then renders without a thumbnail).
fn load_thumb_texture(ctx: &egui::Context, hash: &str, path: &Path) -> Option<egui::TextureHandle> {
    let img = image::open(path).ok()?.to_rgba8();
    let size = [
        usize::try_from(img.width()).ok()?,
        usize::try_from(img.height()).ok()?,
    ];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture(
        format!("wc-tpl-thumb-{hash}"),
        color,
        egui::TextureOptions::LINEAR,
    ))
}

/// Width (px) reserved at a popup row's right edge for the `ScrollArea`'s
/// floating scrollbar, so the trailing trash button never sits under it. egui's
/// floating scrollbar overlays the content (it does not shrink `available_width`)
/// — without this gutter the bar paints over the trash icon.
const POPUP_SCROLLBAR_GUTTER: f32 = 16.0;

/// Render one template row inside the dropdown: a full-row click target that
/// selects the template, plus a trailing trash button that flips the row into a
/// pending-delete state. The two-step `Delete "name"? [Delete] [Cancel]` confirm
/// is rendered by the caller *below* the (closed) combobox — the popup's default
/// `CloseOnClick` dismisses it the instant the trash button is clicked, so an
/// in-popup confirm would be invisible until the next open. Mutates `confirm`
/// (the pending-delete hash) and the field `v`.
fn render_template_row(
    row: &TemplateRow,
    v: &mut String,
    confirm: &mut Option<String>,
    style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
    // The whole row is one fixed-height click target (so the thumbnail and
    // whitespace select too, not just the text). A fixed height also stops the
    // trailing right-to-left trash layout from expanding each row to the popup
    // height — which is what hid all but one row. The width is inset by the
    // scrollbar gutter so the trash button clears the floating scrollbar.
    let row_h = 40.0_f32;
    let row_w = (ui.available_width() - POPUP_SCROLLBAR_GUTTER).max(0.0);
    let (row_rect, row_resp) =
        ui.allocate_exact_size(egui::vec2(row_w, row_h), egui::Sense::click());
    let row_resp = row_resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(row.label.as_str());

    // Full-row background for selected / hover, so selection reads as a row
    // rather than a text-tight highlight (which looked like a copy/paste
    // selection of the label).
    let bg = if *v == row.managed_path {
        style.accent_weak
    } else if row_resp.hovered() {
        egui::Color32::from_white_alpha(24)
    } else {
        egui::Color32::TRANSPARENT
    };
    if bg != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(row_rect, egui::CornerRadius::same(3), bg);
    }

    // The row content (thumbnail, name, subtext) is *painted*, not built from
    // widgets: a `Label` is selectable (I-beam cursor) and captures hover, which
    // would override the row's pointer cursor over the text. Only the trash gets
    // its own interactive region (so it can be hovered/clicked independently);
    // everything else routes through `row_resp`.
    let pad = 6.0;
    let mut content_left = row_rect.left() + pad;

    // Thumbnail: painted to a fixed 36px box, vertically centred.
    if let Some(tid) = row.thumb {
        let size = 36.0;
        let rect = egui::Rect::from_min_size(
            egui::pos2(content_left, row_rect.center().y - size / 2.0),
            egui::vec2(size, size),
        );
        ui.painter().image(
            tid,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        content_left = rect.right() + 8.0;
    }

    // Trash glyph on the right with a 6px margin. Its own click region so hover
    // recolours it (grey → red, a destructive-action cue) and a click flips the
    // row into delete-confirm without selecting it.
    let trash_size = 20.0;
    let trash_rect = egui::Rect::from_min_size(
        egui::pos2(
            row_rect.right() - pad - trash_size,
            row_rect.center().y - trash_size / 2.0,
        ),
        egui::vec2(trash_size, trash_size),
    );
    let trash_resp = ui
        .interact(trash_rect, row_resp.id.with("trash"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Delete from cache");
    let trash_color = if trash_resp.hovered() {
        style.error_red
    } else {
        style.text_secondary
    };
    ui.painter().text(
        trash_rect.center(),
        egui::Align2::CENTER_CENTER,
        phosphor::TRASH,
        egui::FontId::new(16.0, egui::FontFamily::Name("phosphor".into())),
        trash_color,
    );
    if trash_resp.clicked() {
        *confirm = Some(row.hash.clone());
    }

    // Name + subtext, each elided to the column between the thumbnail and the
    // trash so a long name clips with `…` (full name shows on the row hover).
    let text_w = (trash_rect.left() - 8.0 - content_left).max(0.0);
    let name =
        egui::WidgetText::from(egui::RichText::new(row.label.as_str()).color(style.text_primary))
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                text_w,
                egui::TextStyle::Body,
            );
    let subtext = egui::WidgetText::from(
        egui::RichText::new(row.subtext.as_str())
            .size(10.0)
            .color(style.text_faint),
    )
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        text_w,
        egui::TextStyle::Small,
    );
    let (name_h, sub_h) = (name.size().y, subtext.size().y);
    let gap = 1.0;
    let top = row_rect.center().y - (name_h + gap + sub_h) / 2.0;
    ui.painter()
        .galley(egui::pos2(content_left, top), name, style.text_primary);
    ui.painter().galley(
        egui::pos2(content_left, top + name_h + gap),
        subtext,
        style.text_faint,
    );

    // Clicking anywhere on the row (outside the trash) selects it — guarded so a
    // trash click doesn't also change the selection.
    if row_resp.clicked() && confirm.is_none() {
        v.clone_from(&row.managed_path);
    }
}

/// Render the two-step delete confirm below the (closed) combobox: a prompt line
/// plus `[Delete] [Cancel]`. Lives outside the popup so it stays visible after
/// the trash click closes it. On `Delete`: removes the blob from the store,
/// clears the field if the deleted template was active (the sketch falls back to
/// its default layout), and sets `dirty` so the caller reloads the library.
fn render_template_delete_confirm(
    row: &TemplateRow,
    v: &mut String,
    dir: &Path,
    confirm: &mut Option<String>,
    dirty: &mut bool,
    style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
    // `.truncate()`: grid cells default to `Extend` wrap, so a long name would
    // run the prompt off the right edge of the panel instead of clipping.
    ui.add(
        egui::Label::new(
            egui::RichText::new(format!("Delete \"{}\"?", row.label)).color(style.error_red),
        )
        .truncate(),
    )
    .on_hover_text(row.label.as_str());
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(
                egui::RichText::new("Delete").color(style.error_red),
            ))
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            if let Err(err) = store::delete(dir, &row.hash) {
                tracing::warn!(?err, "template delete failed");
            }
            // Clear the active template reference if it pointed at the blob we
            // just deleted — matched by path OR by a now-missing backing file.
            // The existence check (not bare string equality) heals a divergent
            // or dead path, e.g. a raw source path from the file-picker fallback,
            // which otherwise re-persists and warns "file missing" next launch.
            if store::active_ref_is_stale(v, &row.managed_path) {
                v.clear();
            }
            *dirty = true;
            *confirm = None;
        }
        if ui
            .button("Cancel")
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            *confirm = None;
        }
    });
}

/// Render the template-library picker: a `ComboBox` of cached templates. The
/// pinned top row imports a new image (file dialog → ingest → select); each
/// existing row selects on click, with a trailing trash button that flips the
/// row into a pending-delete state. The `Delete "name"? [Delete] [Cancel]`
/// confirm renders *below* the closed combobox (the popup's default
/// `CloseOnClick` dismisses it on the trash click, so an in-popup confirm would
/// be invisible until reopen). `dirty` is set when an import/delete mutates the
/// store so the caller reloads the in-memory library.
#[expect(
    clippy::too_many_arguments,
    reason = "the template-library widget threads field, ids, dialog filters, rows, dirty, and style"
)]
pub(super) fn render_template_library(
    field: &mut dyn bevy::reflect::PartialReflect,
    storage_key: &'static str,
    field_name: &'static str,
    filter_label: &str,
    extensions: &[&str],
    rows: &[TemplateRow],
    dirty: &mut bool,
    style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
    let Some(v) = field.try_downcast_mut::<String>() else {
        ui.label("(expected String for template path)");
        return;
    };
    // The store dir is resolved the same way the `TemplateLibrary` resource was
    // at startup, so an ingest/delete here targets the dir the caller reloads.
    let dir = templates_dir();

    // Closed-state label: the active template's friendly name, or "(none)".
    let selected_text = rows
        .iter()
        .find(|r| r.managed_path == *v)
        .map_or("(none)", |r| r.label.as_str());

    // Which hash (if any) is mid delete-confirm. Stored in egui memory so it
    // survives frames without a Bevy resource. Read before the combobox so the
    // in-popup trash button can set it and the confirm prompt below the closed
    // combobox can read it.
    let confirm_id = egui::Id::new(("wc-template-confirm", storage_key, field_name));
    let mut confirm: Option<String> = ui.memory(|m| m.data.get_temp(confirm_id));

    // Fill the panel with the picker while keeping the column-3 reset glyph
    // on-panel. `clip_rect().width()` is the scroll viewport's stable width —
    // unlike the grid cell's `available_width()`, which collapses on the first
    // frame inside the auto-shrink scroll area (that was the "first open is too
    // narrow" bug). Reserve room for the label (col 1) and the reset glyph
    // (col 3), then clamp.
    let combo_w = (ui.clip_rect().width() - 170.0).clamp(180.0, 380.0);

    // Render the picker in a child `vertical` so the combobox, the delete
    // confirm, and the status line stack top-to-bottom. This is essential: a
    // Grid cell flows left-to-right, so without a vertical sub-layout the
    // confirm's prompt and `[Delete]`/`[Cancel]` buttons march off the right edge
    // of the panel instead of sitting under the dropdown. `set_max_width` bounds
    // the block to `combo_w` (a long selected name grows the closed button toward
    // `available_width`; the `.width()` below is only a minimum), keeping the
    // column-3 reset glyph on-panel — and being a child ui, the width bound does
    // not leak to the Grid's shared `ui` (reset column, later rows).
    ui.vertical(|ui| {
        ui.set_max_width(combo_w);

        egui::ComboBox::from_id_salt(("wc-template-lib", storage_key, field_name))
            .selected_text(selected_text)
            .width(combo_w)
            .truncate()
            .height(320.0)
            .show_ui(ui, |ui| {
                // Pinned import row: the ＋ glyph (phosphor font, accent) followed by a
                // readable sentence (proportional font). A LayoutJob mixes the two
                // fonts in one widget — a single RichText family would force the Latin
                // text through the icon font and render it garbled.
                let mut import_label = egui::text::LayoutJob::default();
                import_label.append(
                    phosphor::PLUS,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::new(14.0, egui::FontFamily::Name("phosphor".into())),
                        color: style.accent_bright,
                        ..Default::default()
                    },
                );
                import_label.append(
                    "  Import image\u{2026}",
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::new(13.0, egui::FontFamily::Proportional),
                        color: style.text_primary,
                        ..Default::default()
                    },
                );
                if ui
                    .selectable_label(false, import_label)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    // Native-only: rfd's synchronous `FileDialog` does not compile on
                    // wasm. The whole `templates` feature is native (it also pulls the
                    // native-only `image` crate), mirroring `hand-tracking-mediapipe`;
                    // this guard matches `render_file_path`'s own rfd guard.
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        let mut dlg = rfd::FileDialog::new();
                        if !extensions.is_empty() {
                            dlg = dlg.add_filter(filter_label, extensions);
                        }
                        if let Some(path) = dlg.pick_file() {
                            match store::ingest(&dir, &path) {
                                Ok(entry) => {
                                    *v = store::managed_path(&dir, &entry)
                                        .to_string_lossy()
                                        .into_owned();
                                    *dirty = true;
                                }
                                Err(err) => tracing::warn!(?err, "template import failed"),
                            }
                        }
                    }
                }
                ui.separator();

                if rows.is_empty() {
                    ui.label(
                        egui::RichText::new("No templates yet")
                            .italics()
                            .color(style.text_faint),
                    );
                }

                for row in rows {
                    render_template_row(row, v, &mut confirm, style, ui);
                }
            });

        // Two-step delete confirm, rendered BELOW the closed combobox (inside the
        // bounded scope so a long prompt can't widen the column) so it stays
        // visible after the trash click closes the popup (default `CloseOnClick`).
        if let Some(hash) = confirm.clone() {
            match rows.iter().find(|r| r.hash == hash) {
                Some(row) => {
                    render_template_delete_confirm(row, v, &dir, &mut confirm, dirty, style, ui);
                }
                // The pending row vanished (e.g. reconciled away out-of-band);
                // drop the stale confirm so the prompt does not linger.
                None => confirm = None,
            }
        }

        // Honest status: a non-empty active path whose file is gone reads as
        // missing (the sketch falls back to its default layout).
        if !v.is_empty() && !std::path::Path::new(v.as_str()).exists() {
            ui.label(
                egui::RichText::new("file missing, using default")
                    .size(10.0)
                    .color(style.warn_amber),
            );
        }
    });

    // Persist the confirm state for next frame, after the scope releases its
    // borrow of `confirm`.
    ui.memory_mut(|m| match &confirm {
        Some(h) => {
            m.data.insert_temp(confirm_id, h.clone());
        }
        None => {
            m.data.remove::<String>(confirm_id);
        }
    });
}
