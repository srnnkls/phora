//! Fetches that gix's high-level `receive()` cannot express: a filtered pack
//! (`blob:none`) and a pack of explicitly wanted objects.

use std::sync::atomic::AtomicBool;

use gix::protocol::fetch::{
    self as proto, Arguments, Negotiate, RefMap, Shallow, Tags, negotiate, refmap,
};
use gix::protocol::transport::client::blocking_io::{Transport, connect};

use super::{Result, SourceError, SourceName};

#[cfg_attr(feature = "trace", tracing::instrument(skip_all, fields(source = %source)))]
pub(super) fn fetch_blobless(
    source: &SourceName,
    repo: &gix::Repository,
    refspecs: &[&str],
    shallow: &Shallow,
    action: &str,
) -> Result<Session> {
    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| fail(source, "find origin", e))?
        .with_fetch_tags(Tags::None);
    remote
        .replace_refspecs(refspecs.iter().copied(), gix::remote::Direction::Fetch)
        .map_err(|e| fail(source, "set refspecs", e))?;
    let mut session = Session::open(source, &remote, action)?;
    let ref_map = session.ref_map(source, &remote)?;
    let mut negotiate = RefNegotiate::new(repo, &ref_map, shallow);
    session.receive(source, repo, &mut negotiate, shallow)?;
    update_refs(source, repo, &ref_map)?;
    Ok(session)
}

#[cfg_attr(feature = "trace", tracing::instrument(skip_all, fields(source = %source, objects = objects.len())))]
pub(super) fn fetch_objects(
    source: &SourceName,
    repo: &gix::Repository,
    objects: &[gix::ObjectId],
    session: Option<Session>,
) -> Result<()> {
    if objects.is_empty() {
        return Ok(());
    }
    let mut session = if let Some(session) = session {
        session
    } else {
        let remote = repo
            .find_remote("origin")
            .map_err(|e| fail(source, "find origin", e))?;
        Session::open(source, &remote, "fetch blobs")?
    };
    let mut negotiate = WantObjects { objects };
    session.receive(source, repo, &mut negotiate, &Shallow::NoChange)
}

fn fail(source: &SourceName, action: &str, error: impl std::fmt::Display) -> SourceError {
    SourceError::Source(format!("{action} for {source}: {error}"))
}

fn user_agent() -> (&'static str, Option<std::borrow::Cow<'static, str>>) {
    (
        "agent",
        Some(format!("phora/{}", env!("CARGO_PKG_VERSION")).into()),
    )
}

pub(super) struct Session {
    transport: gix::protocol::SendFlushOnDrop<Box<dyn Transport + Send>>,
    handshake: gix::protocol::Handshake,
}

impl Session {
    #[expect(
        clippy::result_large_err,
        reason = "the credential callback must return gix_credentials::protocol::Result"
    )]
    fn open(source: &SourceName, remote: &gix::Remote<'_>, action: &str) -> Result<Self> {
        let failed = |error: &dyn std::fmt::Display| {
            SourceError::Source(format!("{action} {source}: {error}"))
        };
        let repo = remote.repo();
        let (url, version) = remote
            .sanitized_url_and_version(gix::remote::Direction::Fetch)
            .map_err(|e| failed(&e))?;
        let ssh = if url.scheme == gix::url::Scheme::Ssh {
            repo.ssh_connect_options().map_err(|e| failed(&e))?
        } else {
            connect::Options::default().ssh
        };
        let transport = connect::connect(
            url.clone(),
            connect::Options {
                version,
                ssh,
                trace: false,
            },
        )
        .map_err(|e| failed(&e))?;
        let mut transport = gix::protocol::SendFlushOnDrop::new(transport, false);
        if let Some(options) = repo
            .transport_options(
                gix::bstr::BStr::new(&url.to_bstring()),
                Some("origin".into()),
            )
            .map_err(|e| failed(&e))?
        {
            transport
                .inner
                .configure(&*options)
                .map_err(|e| failed(&e))?;
        }
        let (mut cascade, _, prompt) = repo
            .config_snapshot()
            .credential_helpers(url)
            .map_err(|e| failed(&e))?;
        let handshake = gix::protocol::handshake(
            &mut transport.inner,
            gix::protocol::transport::Service::UploadPack,
            move |credential| cascade.invoke(credential, prompt.clone()),
            Vec::new(),
            &mut gix::progress::Discard,
        )
        .map_err(|e| failed(&e))?;
        Ok(Self {
            transport,
            handshake,
        })
    }

    fn ref_map(&mut self, source: &SourceName, remote: &gix::Remote<'_>) -> Result<RefMap> {
        let context = refmap::init::Context {
            fetch_refspecs: remote.refspecs(gix::remote::Direction::Fetch).to_vec(),
            extra_refspecs: Vec::new(),
        };
        self.handshake
            .prepare_lsrefs_or_extract_refmap(user_agent(), true, context)
            .map_err(|e| fail(source, "list refs", e))?
            .fetch_blocking(gix::progress::Discard, &mut self.transport.inner, false)
            .map_err(|e| fail(source, "list refs", e))
    }

    fn receive(
        &mut self,
        source: &SourceName,
        repo: &gix::Repository,
        negotiate: &mut impl Negotiate,
        shallow: &Shallow,
    ) -> Result<()> {
        let pack_dir = repo.objects.store_ref().path().join("pack");
        let write_options = gix::odb::pack::bundle::write::Options {
            object_hash: repo.object_hash(),
            ..Default::default()
        };
        let mut keep_path = None;
        gix::protocol::fetch(
            negotiate,
            |reader, progress, interrupt: &AtomicBool| {
                let outcome = gix::odb::pack::Bundle::write_to_directory(
                    reader,
                    Some(&pack_dir),
                    progress,
                    interrupt,
                    Some(Box::new(repo.objects.clone())),
                    write_options,
                )?;
                keep_path = outcome.keep_path;
                Ok::<_, gix::odb::pack::bundle::write::Error>(true)
            },
            gix::progress::Discard,
            &gix::interrupt::IS_INTERRUPTED,
            proto::Context {
                handshake: &mut self.handshake,
                transport: &mut self.transport.inner,
                user_agent: user_agent(),
                trace_packetlines: false,
            },
            proto::Options {
                shallow_file: repo.shallow_file(),
                shallow,
                tags: Tags::None,
                reject_shallow_remote: false,
            },
        )
        .map_err(|e| fail(source, "fetch pack", e))?;
        if let Some(keep) = keep_path {
            std::fs::remove_file(&keep).map_err(|e| fail(source, "remove pack keep file", e))?;
        }
        Ok(())
    }
}

struct RefNegotiate<'a> {
    repo: gix::Repository,
    graph: gix::negotiate::Graph<'a, 'a>,
    negotiator: Box<dyn gix::negotiate::Negotiator>,
    ref_map: &'a RefMap,
    shallow: &'a Shallow,
}

impl<'a> RefNegotiate<'a> {
    fn new(repo: &'a gix::Repository, ref_map: &'a RefMap, shallow: &'a Shallow) -> Self {
        let mut graph_repo = repo.clone();
        graph_repo.objects.unset_object_cache();
        Self {
            graph: repo.revision_graph(None),
            repo: graph_repo,
            negotiator: gix::negotiate::Algorithm::Consecutive.into_negotiator(),
            ref_map,
            shallow,
        }
    }
}

impl Negotiate for RefNegotiate<'_> {
    fn mark_complete_and_common_ref(
        &mut self,
    ) -> std::result::Result<negotiate::Action, negotiate::Error> {
        negotiate::mark_complete_and_common_ref(
            &self.repo.objects,
            &self.repo.refs,
            || {
                Ok::<_, std::convert::Infallible>(std::iter::empty::<(
                    gix::refs::file::Store,
                    gix::OdbHandle,
                )>())
            },
            &mut *self.negotiator,
            &mut self.graph,
            self.ref_map,
            self.shallow,
            negotiate::make_refmapping_ignore_predicate(Tags::None, self.ref_map),
        )
    }

    fn add_wants(&mut self, arguments: &mut Arguments, remote_ref_target_known: &[bool]) -> bool {
        let added = negotiate::add_wants(
            &self.repo.objects,
            arguments,
            self.ref_map,
            remote_ref_target_known,
            self.shallow,
            negotiate::make_refmapping_ignore_predicate(Tags::None, self.ref_map),
        );
        if added && arguments.can_use_filter() {
            arguments.filter("blob:none");
        }
        added
    }

    fn one_round(
        &mut self,
        state: &mut negotiate::one_round::State,
        arguments: &mut Arguments,
        previous_response: Option<&proto::Response>,
    ) -> std::result::Result<(negotiate::Round, bool), negotiate::Error> {
        negotiate::one_round(
            &mut *self.negotiator,
            &mut self.graph,
            state,
            arguments,
            previous_response,
        )
    }
}

struct WantObjects<'a> {
    objects: &'a [gix::ObjectId],
}

impl Negotiate for WantObjects<'_> {
    fn mark_complete_and_common_ref(
        &mut self,
    ) -> std::result::Result<negotiate::Action, negotiate::Error> {
        Ok(negotiate::Action::MustNegotiate {
            remote_ref_target_known: Vec::new(),
        })
    }

    fn add_wants(&mut self, arguments: &mut Arguments, _remote_ref_target_known: &[bool]) -> bool {
        for object in self.objects {
            arguments.want(object);
        }
        true
    }

    fn one_round(
        &mut self,
        _state: &mut negotiate::one_round::State,
        _arguments: &mut Arguments,
        _previous_response: Option<&proto::Response>,
    ) -> std::result::Result<(negotiate::Round, bool), negotiate::Error> {
        Ok((
            negotiate::Round {
                haves_sent: 0,
                in_vain: 0,
                haves_to_send: 0,
                previous_response_had_at_least_one_in_common: false,
            },
            true,
        ))
    }
}

fn update_refs(source: &SourceName, repo: &gix::Repository, ref_map: &RefMap) -> Result<()> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit};
    let mut edits = Vec::new();
    for mapping in &ref_map.mappings {
        let Some(local) = &mapping.local else {
            continue;
        };
        let object = match &mapping.remote {
            refmap::Source::ObjectId(id) => *id,
            refmap::Source::Ref(remote) => match remote.unpack() {
                (_, Some(id), _) => id.to_owned(),
                (name, None, _) => {
                    return Err(SourceError::Source(format!(
                        "remote ref {name} of {source} is unborn"
                    )));
                }
            },
        };
        edits.push(RefEdit {
            change: Change::Update {
                log: LogChange::default(),
                expected: PreviousValue::Any,
                new: gix::refs::Target::Object(object),
            },
            name: gix::refs::FullName::try_from(local.clone())
                .map_err(|e| fail(source, "name local ref", e))?,
            deref: false,
        });
    }
    if !edits.is_empty() {
        repo.edit_references(edits)
            .map_err(|e| fail(source, "update refs", e))?;
    }
    Ok(())
}
