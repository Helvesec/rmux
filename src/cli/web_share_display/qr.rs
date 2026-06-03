use qrcode::{Color as QrColor, EcLevel, QrCode};

pub(super) fn half_block_qr(data: &str) -> Result<Vec<String>, qrcode::types::QrError> {
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L)?;
    let quiet_zone = 2usize;
    let size = code.width() + quiet_zone * 2;
    let mut lines = Vec::new();

    for y in (0..size).step_by(2) {
        let mut line = String::new();
        for x in 0..size {
            let top = qr_dark(&code, quiet_zone, x, y);
            let bottom = qr_dark(&code, quiet_zone, x, y + 1);
            line.push(match (top, bottom) {
                (false, false) => ' ',
                (true, false) => '▀',
                (false, true) => '▄',
                (true, true) => '█',
            });
        }
        lines.push(line);
    }

    Ok(lines)
}

fn qr_dark(code: &QrCode, quiet_zone: usize, x: usize, y: usize) -> bool {
    if x < quiet_zone || y < quiet_zone {
        return false;
    }
    let qx = x - quiet_zone;
    let qy = y - quiet_zone;
    qx < code.width() && qy < code.width() && code[(qx, qy)] == QrColor::Dark
}
