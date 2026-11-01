use tsm_lab::checks::*;
use tsm_lab::Variant;
fn main() {
    let mut rows = Vec::new();
    rows.extend(check_tonal_attacks(Variant::Full, &[0.5, 2.0, 4.0], true));
    rows.extend(check_tonal_attacks(Variant::Full, &[2.0, 4.0], false));
    rows.extend(check_tonal_attacks(Variant::Pv, &[2.0, 4.0], false));
    rows.extend(check_tonal_attacks(Variant::Hptsm, &[2.0, 4.0], false));
    rows.extend(check_attacks(Variant::Pv, &[2.0], false));
    rows.extend(check_attacks(Variant::Hptsm, &[2.0], false));
    rows.extend(check_attacks(Variant::Full, &[2.0], false));
    print_table(&rows);
}
