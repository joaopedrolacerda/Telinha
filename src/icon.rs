// Ícone do telinha desenhado em código (monitor na cor de destaque com um "play" verde).
// Usado pela janela (main.rs) e pelo build.rs, que gera o .ico embutido no .exe.
// Sem dependências: este arquivo é incluído com `include!` no build.rs.

/// RGBA `n`×`n`, com antisserrilhado (4×4 amostras por pixel).
pub fn draw_icon(n: usize) -> Vec<u8> {
    const SS: usize = 4;
    let scale = 64.0 / n as f32;
    let mut rgba = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let (mut r, mut g, mut b, mut hits) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) * scale;
                    let fy = (y as f32 + (sy as f32 + 0.5) / SS as f32) * scale;
                    if let Some([cr, cg, cb]) = icon_sample(fx, fy) {
                        r += cr;
                        g += cg;
                        b += cb;
                        hits += 1.0;
                    }
                }
            }
            if hits > 0.0 {
                let px = &mut rgba[(y * n + x) * 4..][..4];
                px[0] = (r / hits) as u8;
                px[1] = (g / hits) as u8;
                px[2] = (b / hits) as u8;
                px[3] = (hits / (SS * SS) as f32 * 255.0) as u8;
            }
        }
    }
    rgba
}

/// Cor no ponto (x, y) de uma grade 64×64, ou `None` se transparente.
fn icon_sample(x: f32, y: f32) -> Option<[f32; 3]> {
    const ACCENT: [f32; 3] = [99.0, 102.0, 241.0];
    const SCREEN: [f32; 3] = [30.0, 33.0, 43.0];
    const PLAY: [f32; 3] = [34.0, 197.0, 94.0];

    let round = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let cx = x.clamp(x0 + r, x1 - r);
        let cy = y.clamp(y0 + r, y1 - r);
        x >= x0 && x <= x1 && y >= y0 && y <= y1 && (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    let play = x >= 26.0 && x <= 40.0 && (y - 28.0).abs() <= 8.5 - (x - 26.0) * 0.6;

    if play {
        Some(PLAY)
    } else if round(11.0, 15.0, 53.0, 41.0, 3.0) {
        Some(SCREEN)
    } else if round(6.0, 10.0, 58.0, 46.0, 7.0)
        || round(27.0, 45.0, 37.0, 53.0, 1.0)
        || round(17.0, 51.0, 47.0, 57.0, 3.0)
    {
        Some(ACCENT)
    } else {
        None
    }
}
