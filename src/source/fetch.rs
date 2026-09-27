//! Fetches that gix's high-level `receive()` cannot express: a filtered pack
//! (`blob:none`) and a pack of explicitly wanted objects.

use std::io::IsTerminal;
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

/// Credential callback for `url`, carrying the `credential.<url>.username` that gix 0.84
/// resolves but drops before invoking helpers; without it, multi-account helpers
/// (Git Credential Manager) prompt for an account.
#[expect(
    clippy::result_large_err,
    reason = "the credential callback must return gix_credentials::protocol::Result"
)]
pub(super) fn credentials(
    repo: &gix::Repository,
    url: gix::Url,
) -> std::result::Result<
    impl FnMut(gix::credentials::helper::Action) -> gix::credentials::protocol::Result + use<>,
    gix::config::credential_helpers::Error,
> {
    let (mut cascade, configured, mut prompt) = repo.config_snapshot().credential_helpers(url)?;
    prompt.mode = prompt_mode(
        prompt.mode,
        std::io::stdin().is_terminal(),
        std::env::var_os("GIT_TERMINAL_PROMPT").is_some(),
    );
    let user = configured
        .context()
        .and_then(|ctx| ctx.url.as_ref())
        .and_then(|url| gix::url::parse(url.as_ref()).ok())
        .and_then(|url| url.user().map(ToOwned::to_owned));
    Ok(move |mut action| {
        if let Some(user) = &user {
            add_username(&mut action, user);
        }
        cascade.invoke(action, prompt.clone())
    })
}

pub(super) fn use_configured_username<T: Transport>(
    connection: &mut gix::remote::Connection<'_, '_, T>,
) -> std::result::Result<(), gix::config::credential_helpers::Error> {
    let Some(url) = connection
        .remote()
        .url(gix::remote::Direction::Fetch)
        .cloned()
    else {
        return Ok(());
    };
    let authenticate = credentials(connection.remote().repo(), url)?;
    connection.set_credentials(authenticate);
    Ok(())
}

fn prompt_mode(mode: gix::prompt::Mode, attended: bool, explicit: bool) -> gix::prompt::Mode {
    if attended || explicit {
        mode
    } else {
        gix::prompt::Mode::Disable
    }
}

fn add_username(action: &mut gix::credentials::helper::Action, user: &str) {
    let Some(ctx) = action.context_mut() else {
        return;
    };
    let Some(mut url) = ctx
        .url
        .as_ref()
        .and_then(|url| gix::url::parse(url.as_ref()).ok())
    else {
        return;
    };
    if url.user().is_none() {
        url.set_user(Some(user.to_owned()));
        ctx.url = Some(url.to_bstring());
    }
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
        let credentials_url = remote
            .url(gix::remote::Direction::Fetch)
            .cloned()
            .unwrap_or(url);
        let authenticate = credentials(repo, credentials_url).map_err(|e| failed(&e))?;
        let handshake = gix::protocol::handshake(
            &mut transport.inner,
            gix::protocol::transport::Service::UploadPack,
            authenticate,
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

#[cfg(test)]
mod tests {
    use super::{add_username, prompt_mode};
    use gix::credentials::helper::Action;
    use gix::prompt::Mode;

    fn action_url(action: &Action) -> String {
        action
            .context()
            .and_then(|ctx| ctx.url.as_ref())
            .expect("get action carries a url")
            .to_string()
    }

    #[test]
    fn add_username_fills_missing_user() {
        let mut action = Action::get_for_url("https://github.com/owner/repo.git");
        add_username(&mut action, "octo");
        assert_eq!(
            action_url(&action),
            "https://octo@github.com/owner/repo.git"
        );
    }

    #[test]
    fn add_username_keeps_url_user() {
        let mut action = Action::get_for_url("https://first@github.com/owner/repo.git");
        add_username(&mut action, "octo");
        assert_eq!(
            action_url(&action),
            "https://first@github.com/owner/repo.git"
        );
    }

    #[test]
    fn prompt_mode_disables_the_prompt_without_a_terminal() {
        assert_eq!(prompt_mode(Mode::Hidden, false, false), Mode::Disable);
    }

    #[test]
    fn prompt_mode_keeps_the_prompt_at_a_terminal() {
        assert_eq!(prompt_mode(Mode::Hidden, true, false), Mode::Hidden);
    }

    #[test]
    fn prompt_mode_keeps_an_explicit_choice() {
        assert_eq!(prompt_mode(Mode::Hidden, false, true), Mode::Hidden);
    }
}
