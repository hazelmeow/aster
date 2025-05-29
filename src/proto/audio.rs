use crate::app::app_log;
use rodio::{OutputStream, Sink};
use stream_download::{StreamDownload, storage::temp::TempStorageProvider};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug)]
enum AudioCommand {
    PlayStream(StreamDownload<TempStorageProvider>),
    Pause,
    Stop,
}

#[derive(Debug, Clone)]
pub struct Audio {
    tx: UnboundedSender<AudioCommand>,
}

impl Audio {
    pub fn new() -> anyhow::Result<Self> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        std::thread::spawn(move || {
            let (_stream, stream_handle) = OutputStream::try_default().unwrap();
            let mut sink: Option<Sink> = None;

            loop {
                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        AudioCommand::PlayStream(stream) => {
                            // Stop current playback
                            if let Some(s) = sink.take() {
                                s.stop();
                            }

                            let Ok(source) = rodio::Decoder::new(stream) else {
                                app_log!("failed to decode strema");
                                continue;
                            };

                            let new_sink = Sink::try_new(&stream_handle).unwrap();
                            new_sink.append(source);
                            sink = Some(new_sink);
                        }

                        AudioCommand::Pause => {
                            if let Some(s) = &sink {
                                if s.is_paused() {
                                    s.play();
                                } else {
                                    s.pause();
                                }
                            }
                        }

                        AudioCommand::Stop => {
                            if let Some(s) = sink.take() {
                                s.stop();
                            }
                        }
                    }
                }
            }
        });

        Ok(Self { tx })
    }

    pub fn play(&self, stream: StreamDownload<TempStorageProvider>) {
        let _ = self.tx.send(AudioCommand::PlayStream(stream));
    }

    pub fn pause(&self) {
        let _ = self.tx.send(AudioCommand::Pause);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(AudioCommand::Stop);
    }
}

// TODO: tokio task to periodically send poll message, sends state to protocol/app
// TODO: ui/controls
