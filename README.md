# telinha

Compartilhamento de tela simples em Rust, por link temporário do Cloudflare.

1. Você escolhe **uma tela** ou **uma janela de aplicativo** (e se quer mandar o som do PC).
2. O telinha sobe um servidor em `127.0.0.1:<porta>` e abre um **Quick Tunnel** do Cloudflare
   (`https://xxxx.trycloudflare.com`), que não precisa de conta.
3. Quem abrir o link vê a tela no navegador e pode ativar o som.
4. Ao clicar **Parar** (ou fechar o app, ou passar o tempo limite), o túnel é derrubado e o link
   para de funcionar. O `cloudflared` fica preso a um *Job Object* do Windows: mesmo se o telinha
   travar ou for morto, o link cai junto.

## Dois executáveis

- **`telinha.exe`**: interface gráfica em modo escuro. Escolha tela/aplicativo, som,
  quadros/s (15/30/60), resolução, qualidade, link público ou só local, derrubar sozinho após N
  minutos e porta. Ao vivo, mostra o link (com botão de copiar), quantas pessoas estão assistindo,
  o tempo e uma prévia. As opções ficam salvas para a próxima vez.
- **`telinha-cli.exe`**: a mesma coisa pelo terminal (menus ou flags, ver abaixo).

## Requisitos

Compila com Rust + LLVM-MinGW (sem Visual Studio):

```powershell
winget install Rustlang.Rustup
winget install MartinStorsjo.LLVM-MinGW.UCRT
winget install Cloudflare.cloudflared
rustup toolchain install stable-x86_64-pc-windows-gnullvm
rustup default stable-x86_64-pc-windows-gnullvm
```

## Compilar e rodar

```powershell
.\build.ps1
.\target\x86_64-pc-windows-gnullvm\release\telinha.exe
```

### Instalador (MSI)

```powershell
.\installer\build-msi.ps1     # gera telinha-<versão>-x64.msi na raiz
```

Precisa do WiX Toolset 3.14 (binários de github.com/wixtoolset/wix3 em `%LOCALAPPDATA%\wix314`)
e do cloudflared instalado. O MSI instala em `C:\Program Files\telinha` o `telinha.exe`, o
`telinha-cli.exe` e o `cloudflared.exe` (licença Apache-2.0, incluída), cria atalhos no Menu
Iniciar e na Área de Trabalho e aparece em "Aplicativos instalados" para desinstalar. Instalar
uma versão nova (suba `version` no `Cargo.toml`) substitui a anterior.

Linha de comando: sem argumentos, abre menus no terminal. Direto:

```powershell
.\telinha-cli.exe --screen 1 --audio            # tela 1 com som
.\telinha-cli.exe --app chrome --no-audio       # janela que contém "chrome"
.\telinha-cli.exe --screen 1 --audio --minutes 30
```

Opções do `telinha-cli`:

| flag | padrão | o que faz |
|---|---|---|
| `-p, --port` | 8080 | porta local |
| `--fps` | 60 | quadros por segundo (máx. 60) |
| `--quality` | 60 | qualidade do JPEG (1–100) |
| `--max-width` | 1280 | largura máxima enviada |
| `--minutes` | — | derruba o link depois de N minutos |
| `--no-tunnel` | — | só local, sem Cloudflare (para testar) |
| `--screen N` | — | compartilha a tela N sem menu |
| `--app TEXTO` | — | compartilha a janela cujo nome/título contém TEXTO |
| `--audio` / `--no-audio` | — | com ou sem som (todo o som do PC), sem perguntar |
| `--app-audio` | — | com `--app`: só o som do aplicativo compartilhado |
| `--audio-from EXE` | — | só o som destes apps (repita para vários), ex.: `--audio-from chrome.exe` |
| `--list-audio-apps` | — | lista os apps com áudio aberto e sai |
| `--volume` | 100 | volume da transmissão em % (0–200) |
| `--cloudflared` | `cloudflared` | caminho do executável |

## Como funciona

- **Código**: `src/lib.rs` é o núcleo (`session.rs` liga/desliga tudo; `capture.rs`, `audio.rs`,
  `server.rs`, `tunnel.rs`). `src/main.rs` é a interface (egui/eframe) e
  `src/bin/telinha-cli.rs` a linha de comando.

- **Vídeo**: Windows Graphics Capture (`windows-capture`, cópia pela GPU, com cursor) entrega
  quadros no ritmo do monitor; um agendador pega 60/s; 2–4 threads reduzem para `--max-width`
  (`fast_image_resize`) e comprimem em JPEG (`jpeg-encoder`) em paralelo; um sequenciador
  publica na ordem certa → WebSocket. Espectador lento pula quadros em vez de acumular atraso.
  Medido aqui: tela 2560x1440 @120Hz → 60 fps estáveis, inclusive pelo link do Cloudflare.
- **Diagnóstico**: `$env:TELINHA_DEBUG=1` imprime por segundo quantos quadros chegam, entram
  na fila e são descartados.
- **Som**: três modos — todo o PC (`cpal`, loopback WASAPI da saída padrão), só o app
  compartilhado, ou apps escolhidos de uma lista (os que aparecem no Mixer de Volume). Os dois
  últimos usam o loopback por processo do Windows (10 versão 2004+, crate `wasapi`): uma captura
  por app, misturadas a cada 20 ms com um pequeno buffer contra estalos. Volume (0–200%), mudo e
  medidor de nível ajustáveis ao vivo. Sai em PCM 16-bit, pacotes de 20 ms.
- **Segurança**: o link leva um token aleatório (`?t=...`); sem ele o servidor responde 403.
  O servidor só escuta em `127.0.0.1`, então só é alcançável pelo túnel.

## Limitações

- Som por aplicativo precisa do Windows 10 versão 2004 (build 19041) ou mais novo. Um app que
  só começa a tocar som depois de iniciada a transmissão (num processo novo) não entra no modo
  "Escolher aplicativos"; pare e inicie de novo.
- Janelas minimizadas não são capturadas. No Windows 10 aparece uma borda amarela em volta
  do que está sendo capturado (só na sua tela; tirar a borda só é possível no Windows 11).
- A captura só gera quadro quando a imagem muda: tela parada = poucos quadros (e pouca banda).
- É MJPEG, não H.264: a 60 fps em 1280px gasta ~3–4 MB/s (~30 Mbit/s) por espectador, e
  isso sai do seu upload. Se travar, baixe `--quality`, `--max-width` ou `--fps`.
