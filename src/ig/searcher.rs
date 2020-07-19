use std::sync::{mpsc, Arc};

use grep::{
    matcher::LineTerminator,
    regex::RegexMatcher,
    searcher::{Searcher as GrepSearcher, SearcherBuilder as GrepSearcherBuilder, Sink, SinkMatch},
};
use ignore::WalkBuilder;

use super::entries::{FileEntry, Match};

pub enum Event {
    NewEntry(FileEntry),
    SearchingFinished,
}

pub struct SearchConfig {
    pub pattern: String,
    pub path: String, // path: &str -> AsRef<Path>
}

pub struct Searcher {
    inner: Arc<SearcherImpl>,
    tx: mpsc::Sender<Event>,
}

impl Searcher {
    pub fn new(config: SearchConfig, tx: mpsc::Sender<Event>) -> Self {
        Self {
            inner: Arc::new(SearcherImpl::new(config)),
            tx,
        }
    }

    pub fn search(&self) {
        let local_self = self.inner.clone();
        let tx_local = self.tx.clone();
        let _ = std::thread::spawn(move || {
            // handle error?
            match local_self.run(tx_local.clone()) {
                Ok(_) => (),
                Err(_) => (),
            }
            tx_local.send(Event::SearchingFinished);
        });
    }
}

struct SearcherImpl {
    config: SearchConfig,
}

impl SearcherImpl {
    pub fn new(config: SearchConfig) -> Self {
        Self { config }
    }

    pub fn run(&self, tx2: mpsc::Sender<Event>) -> Result<(), Box<dyn std::error::Error>> {
        let matcher = RegexMatcher::new_line_matcher(&self.config.pattern)?;
        let builder = WalkBuilder::new(&self.config.path);

        let walk_parallel = builder.build_parallel();
        walk_parallel.run(move || {
            let pattern = self.config.pattern.clone(); // this is _very_ lame
            let tx = tx2.clone();
            let matcher = matcher.clone();
            let mut grep_searcher = GrepSearcherBuilder::new()
                //.binary_detection(BinaryDetection::quit(b'\x00')) // from simplegrep - check it
                .line_terminator(LineTerminator::byte(b'\n'))
                .line_number(true)
                .multi_line(false)
                .build();

            Box::new(move |result| {
                let dir_entry = match result {
                    Err(_err) => {
                        //eprintln!("{}", err);
                        return ignore::WalkState::Continue;
                    }
                    Ok(entry) => {
                        if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                            return ignore::WalkState::Continue;
                        }
                        entry
                    }
                };

                let mut matches_in_entry = Vec::new();

                // handle error like in simplegrep
                let _ = grep_searcher.search_path(
                    &matcher,
                    dir_entry.path(),
                    MatchesSink(|_, sink_match| {
                        let line_number = sink_match.line_number().unwrap();
                        let text = std::str::from_utf8(sink_match.bytes());
                        if let Ok(t) = text {
                            let span = if let Some(byte_offset) = t.find(&pattern) {
                                Some((byte_offset, byte_offset + pattern.len()))
                            } else {
                                None
                            };
                            let m = Match::new(line_number, t, span);
                            matches_in_entry.push(m);
                        }
                        Ok(true)
                    }),
                );

                if !matches_in_entry.is_empty() {
                    // this can fail if app exits right when event is send
                    tx.send(Event::NewEntry(FileEntry::new(
                        dir_entry.path().to_str().unwrap(),
                        matches_in_entry,
                    )))
                    .unwrap();
                }

                ignore::WalkState::Continue
            })
        });

        Ok(())
    }
}

struct MatchesSink<F>(pub F)
where
    F: FnMut(&GrepSearcher, &SinkMatch) -> Result<bool, std::io::Error>;

impl<F> Sink for MatchesSink<F>
where
    F: FnMut(&GrepSearcher, &SinkMatch) -> Result<bool, std::io::Error>,
{
    type Error = std::io::Error;

    fn matched(
        &mut self,
        searcher: &GrepSearcher,
        sink_match: &SinkMatch,
    ) -> Result<bool, std::io::Error> {
        (self.0)(searcher, sink_match)
    }
}