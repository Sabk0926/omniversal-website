//! Server-Sent Events reader.
//!
//! Only the subset llama.cpp emits: `data:` lines, blank line ends an event,
//! `[DONE]` terminates the stream. Comment lines (`:`) are keep-alives and are
//! skipped. Named events and `id:`/`retry:` fields are parsed and ignored,
//! because ignoring an unknown field is better than failing a stream over it.

use std::io::{BufRead, BufReader, Read};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, when the server sent one.
    pub name: Option<String>,
    /// Concatenated `data:` lines, joined by newline as the spec requires.
    pub data: String,
}

/// Iterator over events. Ends at `[DONE]` or end of stream.
#[derive(Debug)]
pub struct SseReader<R: BufRead> {
    reader: R,
    finished: bool,
}

impl<R: Read> SseReader<BufReader<R>> {
    pub fn from_read(reader: R) -> SseReader<BufReader<R>> {
        SseReader::new(BufReader::new(reader))
    }
}

impl<R: BufRead> SseReader<R> {
    pub fn new(reader: R) -> SseReader<R> {
        SseReader {
            reader,
            finished: false,
        }
    }

    fn next_event(&mut self) -> std::io::Result<Option<SseEvent>> {
        let mut data_lines: Vec<String> = Vec::new();
        let mut name: Option<String> = None;

        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line)?;
            if read == 0 {
                // Stream ended. Emit a partial event if one was accumulating,
                // rather than dropping tokens the server did send.
                self.finished = true;
                return Ok(if data_lines.is_empty() {
                    None
                } else {
                    Some(SseEvent {
                        name,
                        data: data_lines.join("\n"),
                    })
                });
            }

            let line = line.trim_end_matches(['\r', '\n']);

            if line.is_empty() {
                if data_lines.is_empty() && name.is_none() {
                    continue; // blank line between events, or leading padding
                }
                return Ok(Some(SseEvent {
                    name,
                    data: data_lines.join("\n"),
                }));
            }

            if line.starts_with(':') {
                continue; // comment / keep-alive
            }

            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line, ""),
            };

            match field {
                "data" => {
                    if value == "[DONE]" {
                        self.finished = true;
                        return Ok(if data_lines.is_empty() {
                            None
                        } else {
                            Some(SseEvent {
                                name,
                                data: data_lines.join("\n"),
                            })
                        });
                    }
                    data_lines.push(value.to_string());
                }
                "event" => name = Some(value.to_string()),
                _ => {} // id, retry, anything else: ignored on purpose
            }
        }
    }
}

impl<R: BufRead> Iterator for SseReader<R> {
    type Item = std::io::Result<SseEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.next_event() {
            Ok(Some(event)) => Some(Ok(event)),
            Ok(None) => None,
            Err(e) => {
                self.finished = true;
                Some(Err(e))
            }
        }
    }
}
