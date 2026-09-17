//! Soniox BYOK real-time transcription.
//! https://soniox.com/docs/api-reference/stt/websocket-api
//!
//! Tokens may be subwords. Keep confirmed tokens until an endpoint so the
//! native clients, which insert spaces between final segments, cannot split
//! words or detach punctuation. The full current utterance is previewed live.

use super::config::{bool_field, num_field, present, str_field};
use super::session::SessionState;
use super::{
    AudioFraming, LiveConfig, LiveConnect, LiveError, LiveErrorKind, LiveEvent, LiveFrame, StopStep,
};
use serde_json::json;

pub(super) fn connect(config: &LiveConfig) -> Result<LiveConnect, LiveError> {
    let api_key = present(&config.api_key).ok_or(LiveError::MissingCredential)?;
    let mut setup = json!({
        "api_key": api_key.trim(),
        "model": "stt-rt-v5",
        "audio_format": "pcm_s16le",
        "sample_rate": 16000,
        "num_channels": 1,
        "enable_endpoint_detection": true,
    });
    if let Some(language) = super::normalize_language(config.language.as_deref()) {
        setup["language_hints"] = json!([language]);
    }
    let terms = crate::helpers::keyword_boost_terms(&config.vocabulary, Some(100));
    if !terms.is_empty() {
        setup["context"] = json!({"terms": terms});
    }
    Ok(LiveConnect {
        url: "wss://stt-rt.soniox.com/transcribe-websocket".into(),
        headers: vec![],
        subprotocols: vec![],
        sample_rate: 16_000,
        framing: AudioFraming::Binary,
        start_frames: vec![LiveFrame::text(setup.to_string())],
        // Soniox has no acknowledgement frame; audio follows configuration.
        session_starts_on_open: true,
    })
}

pub(super) fn stop_sequence() -> Vec<StopStep> {
    vec![
        StopStep::SendText {
            text: String::new(),
        },
        StopStep::WaitForSessionComplete { timeout_ms: 10_000 },
        StopStep::Close,
    ]
}

pub(super) fn parse(state: &mut SessionState, root: &serde_json::Value) -> LiveEvent {
    if let Some(code) = root.get("error_code").and_then(serde_json::Value::as_u64) {
        // Include a stable classification prefix for clients that classify the
        // message instead of consuming LiveErrorKind. Never echo setup/key data.
        let (prefix, kind) = match code {
            401 | 403 => ("Unauthorized", Some(LiveErrorKind::Unauthorized)),
            402 => ("Payment required", Some(LiveErrorKind::QuotaExceeded)),
            429 => ("Rate limit reached", Some(LiveErrorKind::RateLimited)),
            400 => ("Rejected the session setup", None),
            _ => ("Streaming transcription failed", None),
        };
        return LiveEvent::Error {
            message: format!(
                "Soniox: {prefix}. {}",
                str_field(root, "error_message").unwrap_or("Please try again.")
            ),
            kind,
        };
    }

    let mut completed = String::new();
    let mut provisional = String::new();
    if let Some(tokens) = root.get("tokens").and_then(serde_json::Value::as_array) {
        for token in tokens {
            let Some(text) = str_field(token, "text") else {
                continue;
            };
            let is_final = bool_field(token, "is_final") == Some(true);
            if matches!(text, "<end>" | "<fin>") {
                if is_final {
                    completed.push_str(&std::mem::take(&mut state.soniox_utterance));
                }
            } else if is_final {
                state.soniox_utterance.push_str(text);
            } else {
                provisional.push_str(text);
            }
        }
    }

    if bool_field(root, "finished") == Some(true) {
        completed.push_str(&std::mem::take(&mut state.soniox_utterance));
        let duration_seconds = num_field(root, "total_audio_proc_ms") / 1000.0;
        return if completed.trim().is_empty() {
            LiveEvent::SessionComplete {
                duration_seconds,
                credits_used: 0.0,
            }
        } else {
            LiveEvent::FinalTranscriptAndSessionComplete {
                text: completed,
                duration_seconds,
                credits_used: 0.0,
            }
        };
    }
    if !completed.trim().is_empty() {
        // A following utterance's confirmed tokens stay buffered. Its preview
        // resumes on the next response; a frame yields one protocol event.
        return LiveEvent::FinalTranscript { text: completed };
    }
    if root.get("tokens").is_some() {
        return LiveEvent::PartialTranscript {
            text: format!("{}{provisional}", state.soniox_utterance),
        };
    }
    LiveEvent::Ignore
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{classify_error_message, LiveErrorOutcome, LiveProvider, LiveSession};

    fn session() -> LiveSession {
        let mut config = LiveConfig::new(LiveProvider::Soniox);
        config.api_key = Some("test-soniox-key".into());
        LiveSession::new(config)
    }

    #[test]
    fn setup_uses_binary_pcm_and_existing_key_with_language_and_terms() {
        let mut config = LiveConfig::new(LiveProvider::Soniox);
        config.api_key = Some("  test-soniox-key  ".into());
        config.language = Some("no-NO".into());
        config.vocabulary = vec!["HyperWhisper".into(), "hyperwhisper".into(), "  ".into()];
        // A batch-model preference must never reach the real-time endpoint.
        config.model = Some("stt-async-v5".into());
        let connect = LiveSession::new(config).connect().unwrap();
        assert_eq!(connect.url, "wss://stt-rt.soniox.com/transcribe-websocket");
        assert!(connect.headers.is_empty());
        assert!(connect.subprotocols.is_empty());
        assert!(connect.session_starts_on_open);
        assert_eq!(connect.sample_rate, 16_000);
        assert_eq!(connect.framing, AudioFraming::Binary);
        assert_eq!(connect.start_frames.len(), 1);
        assert!(!connect.start_frames[0].binary);
        let setup: serde_json::Value = serde_json::from_str(&connect.start_frames[0].data).unwrap();
        assert_eq!(
            setup,
            json!({
                "api_key": "test-soniox-key", "model": "stt-rt-v5",
                "audio_format": "pcm_s16le", "sample_rate": 16000, "num_channels": 1,
                "enable_endpoint_detection": true, "language_hints": ["no"],
                "context": {"terms": ["HyperWhisper"]}
            })
        );
    }

    #[test]
    fn auto_language_still_accepts_vocabulary_and_blank_credentials_fail() {
        let mut config = LiveConfig::new(LiveProvider::Soniox);
        config.api_key = Some("  ".into());
        assert_eq!(
            LiveSession::new(config.clone()).connect(),
            Err(LiveError::MissingCredential)
        );
        config.api_key = Some("test-key".into());
        config.language = Some("AUTO".into());
        config.vocabulary = vec!["Soniox".into()];
        let connect = LiveSession::new(config).connect().unwrap();
        let setup: serde_json::Value = serde_json::from_str(&connect.start_frames[0].data).unwrap();
        assert!(setup.get("language_hints").is_none());
        assert_eq!(setup["context"]["terms"], json!(["Soniox"]));
    }

    #[test]
    fn subwords_and_revisions_are_joined_without_duplicate_text_or_extra_spaces() {
        let mut s = session();
        assert_eq!(s.parse(r#"{"tokens":[{"text":"Hy","is_final":true},{"text":"per whisper","is_final":false}]}"#),
            LiveEvent::PartialTranscript { text: "Hyper whisper".into() });
        assert_eq!(
            s.parse(
                r#"{"tokens":[{"text":"per","is_final":true},{"text":"Whisper","is_final":false}]}"#
            ),
            LiveEvent::PartialTranscript {
                text: "HyperWhisper".into()
            }
        );
        assert_eq!(s.parse(r#"{"tokens":[{"text":"Whisper","is_final":true},{"text":"!","is_final":true},{"text":"<end>","is_final":true}]}"#),
            LiveEvent::FinalTranscript { text: "HyperWhisper!".into() });
        assert_eq!(
            s.parse(r#"{"tokens":[{"text":" Next","is_final":false}]}"#),
            LiveEvent::PartialTranscript {
                text: " Next".into()
            }
        );
        assert_eq!(
            s.parse(r#"{"tokens":[]}"#),
            LiveEvent::PartialTranscript {
                text: String::new()
            }
        );
    }

    #[test]
    fn endpoint_does_not_end_session_or_lose_following_utterance() {
        let mut s = session();
        assert_eq!(s.parse(r#"{"tokens":[{"text":"Hei!","is_final":true},{"text":"<end>","is_final":true},{"text":" Hvordan","is_final":true},{"text":" går","is_final":false}]}"#),
            LiveEvent::FinalTranscript { text: "Hei!".into() });
        assert_eq!(s.parse(r#"{"tokens":[{"text":" går det?","is_final":true},{"text":"<end>","is_final":true}]}"#),
            LiveEvent::FinalTranscript { text: " Hvordan går det?".into() });
    }

    #[test]
    fn multiple_endpoints_preserve_token_spacing_including_unspaced_languages() {
        for (first, second, expected) in [
            ("Hello.", " Next sentence.", "Hello. Next sentence."),
            ("你好。", "再见。", "你好。再见。"),
        ] {
            let frame = json!({"tokens": [
                {"text": first, "is_final": true},
                {"text": "<end>", "is_final": true},
                {"text": second, "is_final": true},
                {"text": "<end>", "is_final": true}
            ]});
            assert_eq!(
                session().parse(&frame.to_string()),
                LiveEvent::FinalTranscript {
                    text: expected.into()
                }
            );
        }
    }

    #[test]
    fn stop_sends_empty_frame_and_drains_confirmed_tail_before_completion() {
        let mut s = session();
        s.parse(
            r#"{"tokens":[{"text":"Last","is_final":true},{"text":" wrong","is_final":false}]}"#,
        );
        assert_eq!(
            s.stop_sequence(0),
            vec![
                StopStep::SendText {
                    text: String::new()
                },
                StopStep::WaitForSessionComplete { timeout_ms: 10_000 },
                StopStep::Close
            ]
        );
        assert_eq!(s.parse(r#"{"tokens":[{"text":" words.","is_final":true},{"text":"<fin>","is_final":true}],"finished":true,"total_audio_proc_ms":1234}"#),
            LiveEvent::FinalTranscriptAndSessionComplete { text: "Last words.".into(), duration_seconds: 1.234, credits_used: 0.0 });
        assert_eq!(
            s.parse(r#"{"tokens":[],"finished":true}"#),
            LiveEvent::SessionComplete {
                duration_seconds: 0.0,
                credits_used: 0.0
            }
        );
    }

    #[test]
    fn reconnect_and_reset_forget_old_confirmed_tokens() {
        let mut s = session();
        for reconnect in [false, true] {
            s.parse(r#"{"tokens":[{"text":"old","is_final":true}]}"#);
            if reconnect {
                s.connect().unwrap();
            } else {
                s.reset();
            }
            assert_eq!(
                s.parse(r#"{"tokens":[{"text":"new","is_final":true}],"finished":true}"#),
                LiveEvent::FinalTranscriptAndSessionComplete {
                    text: "new".into(),
                    duration_seconds: 0.0,
                    credits_used: 0.0
                }
            );
        }
    }

    #[test]
    fn auth_and_setup_errors_are_terminal_but_service_failures_are_retryable() {
        for (code, outcome) in [
            (400, LiveErrorOutcome::Terminal),
            (401, LiveErrorOutcome::Terminal),
            (402, LiveErrorOutcome::Terminal),
            (403, LiveErrorOutcome::Terminal),
            (503, LiveErrorOutcome::Transient),
        ] {
            let frame = json!({"error_code": code, "error_message": "Request refused"}).to_string();
            let LiveEvent::Error { message, .. } = session().parse(&frame) else {
                panic!("expected error")
            };
            assert_eq!(classify_error_message(&message), outcome);
        }
    }
}
