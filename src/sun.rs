use chrono::{Datelike, NaiveDate, NaiveTime, Timelike};
use std::f64::consts::PI;
use crate::theme::Theme;

/// 太阳高度角天顶距（官方日出日落定义，含大气折射）。
const ZENITH: f64 = 90.833;

#[derive(Debug, Clone, Copy)]
pub struct SunTimes {
    /// 当地日出时间（小数小时，0~24）；极夜时为 NaN
    pub sunrise: f64,
    /// 当地日落时间（小数小时，0~24）；极昼时为 NaN
    pub sunset: f64,
    /// 极昼（太阳不落）
    pub always_light: bool,
    /// 极夜（太阳不出）
    pub always_dark: bool,
}

fn to_rad(d: f64) -> f64 {
    d * PI / 180.0
}
fn to_deg(r: f64) -> f64 {
    r * 180.0 / PI
}
fn norm360(x: f64) -> f64 {
    let mut r = x % 360.0;
    if r < 0.0 {
        r += 360.0;
    }
    r
}
fn norm24(x: f64) -> f64 {
    let mut r = x % 24.0;
    if r < 0.0 {
        r += 24.0;
    }
    r
}
fn atan_deg(x: f64) -> f64 {
    to_deg(x.atan())
}

/// 把太阳赤经调整到与太阳黄经相同的象限（0~360）。
fn ra_quadrant(l: f64, ra: f64) -> f64 {
    let lq = (l / 90.0).floor() * 90.0;
    let rq = (ra / 90.0).floor() * 90.0;
    let mut ra = ra + (lq - rq);
    if ra < 0.0 {
        ra += 360.0;
    }
    ra
}

/// 计算给定日期/坐标/时区的日出日落（当地小数小时）。
/// 算法参考 "Almanac for Computers"（NOAA 经典实现）。
pub fn sun_times(date: NaiveDate, lat: f64, lon: f64, tz: f64) -> SunTimes {
    let (year, month, day) = (date.year(), date.month() as i32, date.day() as i32);

    // 一年中的第几天
    let n1 = (274 * month) / 9;
    let n2 = ((month + 9) / 12) * (1 + ((year - 4 * (year / 4) + 2) / 3));
    let n = (n1 - n2 + day - 30) as f64;

    let lng_hour = lon / 15.0;

    // 日出 / 日落 的近似时间（单位：天）
    let t_rise = n + (6.0 - lng_hour) / 24.0;
    let t_set = n + (18.0 - lng_hour) / 24.0;

    // 太阳平近点角
    let m_rise = 0.9856 * t_rise - 3.289;
    let m_set = 0.9856 * t_set - 3.289;

    // 太阳真黄经
    let l_rise =
        norm360(m_rise + 1.916 * to_rad(m_rise).sin() + 0.020 * to_rad(2.0 * m_rise).sin() + 282.634);
    let l_set =
        norm360(m_set + 1.916 * to_rad(m_set).sin() + 0.020 * to_rad(2.0 * m_set).sin() + 282.634);

    // 太阳赤经
    let ra_rise = norm360(atan_deg(0.91764 * to_rad(l_rise).tan()));
    let ra_set = norm360(atan_deg(0.91764 * to_rad(l_set).tan()));
    let ra_rise = ra_quadrant(l_rise, ra_rise);
    let ra_set = ra_quadrant(l_set, ra_set);
    let ra_rise_h = ra_rise / 15.0;
    let ra_set_h = ra_set / 15.0;

    // 太阳赤纬
    let sin_dec_rise = 0.39782 * to_rad(l_rise).sin();
    let cos_dec_rise = (1.0 - sin_dec_rise * sin_dec_rise).sqrt();
    let sin_dec_set = 0.39782 * to_rad(l_set).sin();
    let cos_dec_set = (1.0 - sin_dec_set * sin_dec_set).sqrt();

    // 时角余弦
    let cos_h_rise =
        (to_rad(ZENITH).cos() - sin_dec_rise * to_rad(lat).sin()) / (cos_dec_rise * to_rad(lat).cos());
    let cos_h_set =
        (to_rad(ZENITH).cos() - sin_dec_set * to_rad(lat).sin()) / (cos_dec_set * to_rad(lat).cos());

    // 极地情况检测
    let always_light = cos_h_rise < -1.0 && cos_h_set < -1.0; // 极昼
    let always_dark = cos_h_rise > 1.0 && cos_h_set > 1.0; // 极夜

    let h_rise_deg = to_deg(cos_h_rise.max(-1.0).min(1.0).acos());
    let h_set_deg = to_deg(cos_h_set.max(-1.0).min(1.0).acos());

    // 本地平均时
    // 注意：经典 Almanac 算法中，日出用 (360 - H)、日落用 H，二者是不同的
    // 符号约定（源于对时角取 acos 后 sunrise 取补、sunset 取正）。原实现写反会导致
    // 日出/日落时间互换。已用 Python 移植版交叉验证（见仓库 suncheck.py）。
    let t_rise_lmt = (360.0 - h_rise_deg) / 15.0 + ra_rise_h - 0.06571 * t_rise - 6.622;
    let t_set_lmt = h_set_deg / 15.0 + ra_set_h - 0.06571 * t_set - 6.622;

    let ut_rise = norm24(t_rise_lmt - lng_hour);
    let ut_set = norm24(t_set_lmt - lng_hour);
    let local_rise = norm24(ut_rise + tz);
    let local_set = norm24(ut_set + tz);

    SunTimes {
        sunrise: if always_light { f64::NAN } else { local_rise },
        sunset: if always_dark { f64::NAN } else { local_set },
        always_light,
        always_dark,
    }
}

/// 根据当前时间与当日日出日落，决定期望主题。
pub fn desired_theme_for_sun(now: NaiveTime, st: &SunTimes) -> Theme {
    if st.always_light {
        return Theme::Light;
    }
    if st.always_dark {
        return Theme::Dark;
    }
    let nh = now.hour() as f64 + now.minute() as f64 / 60.0 + now.second() as f64 / 3600.0;
    if nh >= st.sunrise && nh < st.sunset {
        Theme::Light
    } else {
        Theme::Dark
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn sun_order_sane() {
        // 北京附近，非极地：日出应早于日落
        let d = NaiveDate::from_ymd_opt(2026, 6, 21).unwrap();
        let st = sun_times(d, 39.9, 116.4, 8.0);
        assert!(!st.always_light && !st.always_dark);
        assert!(st.sunrise < st.sunset, "sunrise={} sunset={}", st.sunrise, st.sunset);
        // 大致范围
        assert!((4.0..=9.0).contains(&st.sunrise), "sunrise out of range: {}", st.sunrise);
        assert!((17.0..=21.0).contains(&st.sunset), "sunset out of range: {}", st.sunset);
    }
}
