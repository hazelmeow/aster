use crate::app::{AppEvent, app_log, event::app_send};
use iroh::NodeId;
use rodio::{OutputStream, Sink, Source};
use std::time::Duration;
use stream_download::{StreamDownload, storage::temp::TempStorageProvider};
use tokio::sync::mpsc::UnboundedSender;

const AUDIO_STATE_FPS: f64 = 5.0;

#[derive(Debug, Clone)]
pub struct AudioState {
    pub label: String,
    pub peer: NodeId,
    pub elapsed: f64,
    pub duration: f64,
    pub paused: bool,
}

#[derive(Debug)]
pub struct AudioTrack {
    pub label: String,
    pub peer_id: NodeId,
    pub stream: StreamDownload<TempStorageProvider>,
}

#[derive(Debug)]
enum AudioCommand {
    PlayTrack(Box<AudioTrack>),
    Pause,
    Stop,
    Seek(f64),

    PollState,
}

#[derive(Debug, Clone)]
pub struct Audio {
    tx: UnboundedSender<AudioCommand>,
}

impl Audio {
    pub fn new() -> anyhow::Result<Self> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        tokio::spawn({
            let tx = tx.clone();
            async move {
                let mut interval =
                    tokio::time::interval(Duration::from_secs_f64(1.0 / AUDIO_STATE_FPS));
                loop {
                    let _ = tx.send(AudioCommand::PollState);
                    interval.tick().await;
                }
            }
        });

        std::thread::spawn(move || {
            let (_stream, stream_handle) = OutputStream::try_default().unwrap();
            let mut sink: Option<Sink> = None;

            let mut current_duration: Option<Duration> = None;
            let mut current_label = String::new();
            let mut current_peer: Option<NodeId> = None;

            loop {
                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        AudioCommand::PlayTrack(track) => {
                            // Stop current playback
                            if let Some(s) = sink.take() {
                                s.stop();
                            }

                            let Ok(source) = rodio::Decoder::new(track.stream) else {
                                app_log!("failed to decode strema");
                                continue;
                            };

                            current_duration = source.total_duration();
                            current_label = track.label;
                            current_peer = Some(track.peer_id);

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

                        AudioCommand::Seek(seek_amount) => {
                            let mut stop = false;

                            if let Some(sink) = &mut sink {
                                let current_pos = sink.get_pos().as_secs_f64();
                                let new_pos = current_pos + seek_amount;

                                let max_pos =
                                    current_duration.unwrap_or(Duration::ZERO).as_secs_f64();
                                if new_pos <= 0.0 {
                                    let _ = sink.try_seek(Duration::ZERO);
                                } else if new_pos >= max_pos {
                                    sink.stop();
                                    stop = true;
                                } else {
                                    let _ = sink.try_seek(Duration::from_secs_f64(new_pos));
                                }
                            }

                            if stop {
                                sink = None;
                            }
                        }

                        AudioCommand::PollState => {
                            let Some(sink) = &sink else {
                                app_send!(AppEvent::PollAudioState(None));
                                continue;
                            };

                            let elapsed = sink.get_pos().as_secs_f64();
                            let duration =
                                current_duration.map(|x| x.as_secs_f64()).unwrap_or(elapsed);

                            let state = AudioState {
                                label: current_label.clone(),
                                peer: current_peer.expect("should be initialized"),
                                elapsed,
                                duration,
                                paused: sink.is_paused(),
                            };

                            app_send!(AppEvent::PollAudioState(Some(state)));
                        }
                    }
                }
            }
        });

        Ok(Self { tx })
    }

    pub fn play(&self, track: AudioTrack) {
        let _ = self.tx.send(AudioCommand::PlayTrack(Box::new(track)));
    }

    pub fn pause(&self) {
        let _ = self.tx.send(AudioCommand::Pause);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(AudioCommand::Stop);
    }

    pub fn seek_backward(&self) {
        let _ = self.tx.send(AudioCommand::Seek(-10.0));
    }

    pub fn seek_forward(&self) {
        let _ = self.tx.send(AudioCommand::Seek(10.0));
    }
}
