//! Gera o ícone (.ico) e as informações de versão e embute nos executáveis do Windows.
use std::path::Path;
use std::process::Command;

include!("src/icon.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out = std::env::var("OUT_DIR").unwrap();
    let out = Path::new(&out);
    std::fs::write(out.join("telinha.ico"), build_ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).unwrap();

    let version = env!("CARGO_PKG_VERSION");
    let mut parts: Vec<&str> = version.split('.').collect();
    parts.resize(4, "0");
    let commas = parts.join(",");
    let rc = format!(
        r#"1 ICON "telinha.ico"
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "041604B0"
    BEGIN
      VALUE "CompanyName", "joaopedrolacerda"
      VALUE "FileDescription", "telinha - compartilhamento de tela"
      VALUE "FileVersion", "{version}"
      VALUE "ProductName", "telinha"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x416, 1200
  END
END
"#
    );
    std::fs::write(out.join("telinha.rc"), rc).unwrap();

    // windres do LLVM-MinGW (está no PATH via build.ps1).
    let obj = out.join("telinha-res.o");
    let status = Command::new("x86_64-w64-mingw32-windres")
        .current_dir(out)
        .args(["-i", "telinha.rc", "-o"])
        .arg(&obj)
        .args(["-O", "coff"])
        .status();
    match status {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", obj.display()),
        _ => println!("cargo:warning=windres não encontrado: o .exe ficará sem ícone embutido"),
    }
}

/// .ico com várias resoluções, cada uma como bitmap 32 bits (BGRA, de baixo para cima).
fn build_ico(sizes: &[usize]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&n| {
            let rgba = draw_icon(n);
            let mask_row = n.div_ceil(32) * 4;
            let mut img = Vec::new();
            // BITMAPINFOHEADER (altura dobrada: cor + máscara)
            for v in [40u32, n as u32, (n * 2) as u32] {
                img.extend_from_slice(&v.to_le_bytes());
            }
            img.extend_from_slice(&1u16.to_le_bytes()); // planes
            img.extend_from_slice(&32u16.to_le_bytes()); // bits
            img.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
            img.extend_from_slice(&((n * n * 4 + mask_row * n) as u32).to_le_bytes());
            img.extend_from_slice(&[0u8; 16]);
            for y in (0..n).rev() {
                for x in 0..n {
                    let p = &rgba[(y * n + x) * 4..][..4];
                    img.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
                }
            }
            img.extend(std::iter::repeat_n(0u8, mask_row * n));
            img
        })
        .collect();

    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&n, img) in sizes.iter().zip(&images) {
        let dim = if n >= 256 { 0 } else { n as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in &images {
        ico.extend_from_slice(img);
    }
    ico
}
