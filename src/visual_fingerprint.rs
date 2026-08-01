pub(crate) fn distance(left: &[u8], right: &[u8]) -> f32 {
    if left.is_empty() || left.len() != right.len() {
        return f32::INFINITY;
    }

    let total: u32 = left
        .iter()
        .zip(right)
        .map(|(left, right)| left.abs_diff(*right) as u32)
        .sum();
    total as f32 / left.len() as f32
}
