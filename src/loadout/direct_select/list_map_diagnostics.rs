use image::{Rgba, RgbaImage};
use tracing::{debug, warn};

use super::{ListMap, SlotId};

const CELL_PADDING: u32 = 2;
const CELL_BORDER: Rgba<u8> = Rgba([64, 64, 64, 255]);
const SELECTED_BORDER: Rgba<u8> = Rgba([255, 220, 32, 255]);
const BACKGROUND: Rgba<u8> = Rgba([8, 8, 8, 255]);

impl Drop for ListMap {
    fn drop(&mut self) {
        let Some(image) = render(self) else {
            return;
        };
        match crate::vision::save_list_map_image(self.item_kind, &image) {
            Ok(path) => debug!(
                item_kind = self.item_kind.label(),
                path = %path.display(),
                mapped_slots = self.slots.len(),
                "temporary list map image saved"
            ),
            Err(error) => warn!(
                item_kind = self.item_kind.label(),
                error = %format!("{error:#}"),
                "failed to save temporary list map image"
            ),
        }
    }
}

fn render(map: &ListMap) -> Option<RgbaImage> {
    let min_col = map.slots.iter().map(|slot| slot.col).min()?;
    let max_col = map.slots.iter().map(|slot| slot.col).max()?;
    let min_top = map
        .slots
        .iter()
        .map(|slot| slot.content_y - slot.sample.center_y)
        .min_by(f32::total_cmp)?;
    let max_bottom = map
        .slots
        .iter()
        .map(|slot| slot.content_y - slot.sample.center_y + slot.sample.height as f32)
        .max_by(f32::total_cmp)?;
    let cell_width = map.slots.iter().map(|slot| slot.sample.width).max()? + 2 * CELL_PADDING;
    let columns = max_col - min_col + 1;
    let height = (max_bottom - min_top).ceil().max(1.0) as u32 + 2 * CELL_PADDING;
    let mut image = RgbaImage::from_pixel(columns * cell_width, height, BACKGROUND);

    for (index, mapped) in map.slots.iter().enumerate() {
        let sample = &mapped.sample;
        let cell_x = (mapped.col - min_col) * cell_width;
        let cell_y = (mapped.content_y - sample.center_y - min_top)
            .round()
            .max(0.0) as u32;
        let x = cell_x + (cell_width - sample.width) / 2;
        let y = cell_y + CELL_PADDING;
        for sample_y in 0..sample.height {
            for sample_x in 0..sample.width {
                let index = sample_y as usize * sample.width as usize + sample_x as usize;
                let value = sample.response[index];
                image.put_pixel(x + sample_x, y + sample_y, Rgba([value, value, value, 255]));
            }
        }
        let border = if map.selected.contains(&SlotId(index)) {
            SELECTED_BORDER
        } else {
            CELL_BORDER
        };
        draw_border(
            &mut image,
            cell_x,
            cell_y,
            cell_width,
            sample.height + 2 * CELL_PADDING,
            border,
        );
    }
    Some(image)
}

fn draw_border(image: &mut RgbaImage, x: u32, y: u32, width: u32, height: u32, color: Rgba<u8>) {
    for px in x..x + width {
        image.put_pixel(px, y, color);
        image.put_pixel(px, y + height - 1, color);
    }
    for py in y..y + height {
        image.put_pixel(x, py, color);
        image.put_pixel(x + width - 1, py, color);
    }
}
