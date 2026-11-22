//! Captura do som do PC (loopback WASAPI): tudo que está tocando na saída padrão.
use anyhow::{anyhow, Result};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use std::sync::mpsc;
use std::thread;
use tokio::sync::broadcast;

/// Tag do pacote de áudio no WebSocket.
pub const TAG_AUDIO: u8 = 2;

#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Enquanto existir, o som continua sendo capturado. Soltar (`drop`) para parar.
pub struct AudioHandle {
    _stop: mpsc::Sender<()>,
}

/// Inicia a captura numa thread própria (o `cpal::Stream` não é `Send`).
/// Pacotes: [TAG_AUDIO, PCM i16 little-endian intercalado...], ~20ms cada.
pub fn start_loopback(tx: broadcast::Sender<Bytes>) -> Result<(AudioInfo, AudioHandle)> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<AudioInfo>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    thread::spawn(move || match build_stream(tx) {
        Ok((stream, info)) => {
            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(anyhow!(e)));
                return;
            }
            let _ = ready_tx.send(Ok(info));
            // Mantém o stream vivo até o AudioHandle ser solto (recv devolve erro).
            let _ = stop_rx.recv();
            drop(stream);
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    });

    let info = ready_rx.recv()??;
    Ok((info, AudioHandle { _stop: stop_tx }))
}

fn build_stream(tx: broadcast::Sender<Bytes>) -> Result<(cpal::Stream, AudioInfo)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("nenhum dispositivo de saída de som"))?;
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let in_channels = config.channels as usize;
    let out_channels = in_channels.min(2);
    let info = AudioInfo {
        sample_rate: config.sample_rate.0,
        channels: out_channels as u16,
    };

    // No WASAPI, abrir um stream de *entrada* num dispositivo de *saída* = loopback.
    let stream = match format {
        SampleFormat::F32 => input::<f32>(&device, &config, in_channels, out_channels, info.sample_rate, tx)?,
        SampleFormat::I16 => input::<i16>(&device, &config, in_channels, out_channels, info.sample_rate, tx)?,
        SampleFormat::U16 => input::<u16>(&device, &config, in_channels, out_channels, info.sample_rate, tx)?,
        SampleFormat::I32 => input::<i32>(&device, &config, in_channels, out_channels, info.sample_rate, tx)?,
        other => return Err(anyhow!("formato de áudio não suportado: {other:?}")),
    };
    Ok((stream, info))
}

fn input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    in_channels: usize,
    out_channels: usize,
    sample_rate: u32,
    tx: broadcast::Sender<Bytes>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let chunk_samples = (sample_rate as usize / 50) * out_channels; // 20ms
    let mut pending: Vec<i16> = Vec::with_capacity(chunk_samples * 2);

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            if tx.receiver_count() == 0 {
                pending.clear();
                return;
            }
            for frame in data.chunks(in_channels) {
                for &s in frame.iter().take(out_channels) {
                    let f: f32 = s.to_sample::<f32>();
                    pending.push((f.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                }
            }
            while pending.len() >= chunk_samples {
                let mut packet = Vec::with_capacity(1 + chunk_samples * 2);
                packet.push(TAG_AUDIO);
                for s in pending.drain(..chunk_samples) {
                    packet.extend_from_slice(&s.to_le_bytes());
                }
                let _ = tx.send(Bytes::from(packet));
            }
        },
        |err| crate::logln!("Áudio: {err}"),
        None,
    )?;
    Ok(stream)
}
