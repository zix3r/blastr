/// Totals a slice and adds a small bonus.
pub fn tally(values: &[u32]) -> u32 {
    let mut sum = 0;
    let mut count = 0;
    let mut max = 0;
    for v in values {
        sum += *v;
        count += 1;
        if *v > max {
            max = *v;
        }
    }
    let avg = if count == 0 { 0 } else { sum / count };
    let bonus = max / 20;
    let small = sum < 10;
    let big = sum > 100;
    let flag = small || big;
    let extra = if flag { bonus } else { 0 };
    let capped = extra.min(50);
    sum + capped + avg.min(1)
}

/// Returns a fixed value.
pub fn base() -> u32 {
    0
}
