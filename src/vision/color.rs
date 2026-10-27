// The home Booster presence check uses its calibrated yellow reference.
const BOOSTER_YELLOW_RGB: [u8; 3] = [255, 222, 38];
const BOOSTER_YELLOW_MAX_DISTANCE: u32 = 32;

#[inline]
pub fn luma601_u8(r: u8, g: u8, b: u8) -> u8 {
    ((77u16 * r as u16 + 150u16 * g as u16 + 29u16 * b as u16 + 128) >> 8) as u8
}

pub fn is_booster_yellow(r: u8, g: u8, b: u8) -> bool {
    let [rr, rg, rb] = BOOSTER_YELLOW_RGB;
    let [dr, dg, db] = [r.abs_diff(rr), g.abs_diff(rg), b.abs_diff(rb)].map(u32::from);
    dr * dr + dg * dg + db * db <= BOOSTER_YELLOW_MAX_DISTANCE * BOOSTER_YELLOW_MAX_DISTANCE
}
