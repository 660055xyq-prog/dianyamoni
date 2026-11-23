//! 纯整数电量百分比计算（对应 daemon.sh calc_battery_pct* / apply_high_soc_floor）。

/// mode0 线性（calc_battery_pct_linear）。
pub fn calc_battery_pct_linear(u: i32, f: i32, c: i32) -> i32 {
    if u >= f {
        return 100;
    }
    if u <= c || f <= c {
        return 0;
    }
    let span_full = f - c;
    ((u - c) * 100 + span_full / 2) / span_full
}

/// mode2 专属曲线 11 节点线性插值（calc_battery_pct_custom）。
pub fn calc_battery_pct_custom(u: i32, curve: &[i32; 11], custom_ok: bool, f: i32, c: i32) -> i32 {
    if !custom_ok {
        return calc_battery_pct_linear(u, f, c);
    }
    if u >= curve[10] {
        return 100;
    }
    if u <= curve[0] {
        return 0;
    }
    for i in 0..10 {
        let vlo = curve[i];
        let vhi = curve[i + 1];
        if u >= vlo && u <= vhi {
            let plo = (i as i32) * 10;
            return plo + (u - vlo) * 10 / (vhi - vlo);
        }
    }
    0
}

/// mode1 锂电分段（calc_battery_pct 的 default 分支）。
pub fn calc_battery_pct_lithium(u: i32, f: i32, c: i32) -> i32 {
    if u >= f {
        return 100;
    }
    if u <= c || f <= c {
        return 0;
    }
    let du = (u - c) / 100;
    let span = (f - c) / 100;
    if span <= 0 {
        return 0;
    }
    let r = du * 1000 / span;
    let (r_lo, r_hi, p_lo) = if r >= 879 {
        (879, 1000, 90)
    } else if r >= 786 {
        (786, 879, 80)
    } else if r >= 700 {
        (700, 786, 70)
    } else if r >= 629 {
        (629, 700, 60)
    } else if r >= 571 {
        (571, 629, 50)
    } else if r >= 529 {
        (529, 571, 40)
    } else if r >= 486 {
        (486, 529, 30)
    } else if r >= 429 {
        (429, 486, 20)
    } else if r >= 321 {
        (321, 429, 10)
    } else {
        (0, 321, 0)
    };
    let seg = r_hi - r_lo;
    let pct = p_lo + ((r - r_lo) * 10 + seg / 2) / seg;
    pct.clamp(0, 100)
}

/// 统一入口（calc_battery_pct）。
pub fn calc_battery_pct(
    u: i32,
    f: i32,
    c: i32,
    curve_mode: i32,
    curve: &[i32; 11],
    custom_ok: bool,
) -> i32 {
    match curve_mode {
        0 => calc_battery_pct_linear(u, f, c),
        2 => calc_battery_pct_custom(u, curve, custom_ok, f, c),
        _ => calc_battery_pct_lithium(u, f, c),
    }
}

/// 高电区温和防跳钳位（apply_high_soc_floor）。
pub fn apply_high_soc_floor(sim_p: i32, sys_p: i32, curve_mode: i32) -> i32 {
    let floor = if curve_mode != 2 && sys_p >= 85 && sim_p >= 80 && sys_p > sim_p {
        sys_p
    } else {
        sim_p
    };
    if floor <= 0 {
        1
    } else {
        floor
    }
}
