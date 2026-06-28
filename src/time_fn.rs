//! Local-time quick answer for the launcher.

use windows::Win32::System::SystemInformation::GetLocalTime;

pub fn eval(input: &str) -> Option<String> {
    let q = input.trim().to_ascii_lowercase();
    if q == "time" || q == "now" || q == "current time" {
        Some(local_time_string())
    } else {
        None
    }
}

fn local_time_string() -> String {
    let t = unsafe { GetLocalTime() };

    let hour24 = t.wHour as u32;
    let minute = t.wMinute as u32;
    let suffix = if hour24 >= 12 { "PM" } else { "AM" };
    let hour12 = match hour24 % 12 {
        0 => 12,
        h => h,
    };

    format!("{hour12}:{minute:02} {suffix}")
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Short date for the top bar, e.g. `"Sat 27 Jun"`.
pub fn bar_date() -> String {
    let t = unsafe { GetLocalTime() };
    let wd = WEEKDAYS.get(t.wDayOfWeek as usize).copied().unwrap_or("");
    let mon = MONTHS
        .get((t.wMonth as usize).saturating_sub(1))
        .copied()
        .unwrap_or("");
    format!("{wd} {} {mon}", t.wDay)
}

/// Clock for the top bar, e.g. `"3:04 PM"`.
pub fn bar_time() -> String {
    local_time_string()
}
