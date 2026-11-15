//! Listing commits.

use blastr_model::CommitInfo;
use gix::traverse::commit::simple::CommitTimeOrder;

use crate::repo::short_id;
use crate::{Error, Repo};

impl Repo {
    /// Commits reachable from `HEAD`, newest first.
    ///
    /// The first `skip` commits are left out and at most `limit` are returned;
    /// a `limit` of zero means no limit. A repository without commits has an
    /// empty list.
    pub fn log(&self, skip: usize, limit: usize) -> Result<Vec<CommitInfo>, Error> {
        let repo = self.repo()?;
        let head = repo.head().map_err(Error::git)?;
        let Some(head_id) = head.id() else {
            return Ok(Vec::new());
        };
        let walk = repo
            .rev_walk([head_id.detach()])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                CommitTimeOrder::NewestFirst,
            ))
            .all()
            .map_err(Error::git)?;
        let mut out = Vec::new();
        let mut skipped = 0;
        for info in walk {
            if skipped < skip {
                skipped += 1;
                continue;
            }
            if limit != 0 && out.len() >= limit {
                break;
            }
            let info = info.map_err(Error::git)?;
            out.push(commit_info(&info)?);
        }
        Ok(out)
    }
}

/// The information about one commit that a listing shows.
fn commit_info(info: &gix::revision::walk::Info<'_>) -> Result<CommitInfo, Error> {
    let commit = info.object().map_err(Error::git)?;
    let author = commit.author().map_err(Error::git)?;
    let name: &[u8] = author.name;
    let time = author.time().map_err(Error::git)?;
    let message = commit.message_raw().map_err(Error::git)?;
    let message: &[u8] = message;
    let subject = message
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    Ok(CommitInfo {
        id: commit.id.to_string(),
        short_id: short_id(commit.id),
        subject: String::from_utf8_lossy(subject).into_owned(),
        author: String::from_utf8_lossy(name).into_owned(),
        time: time.seconds,
        parents: u32::try_from(commit.parent_ids().count()).unwrap_or(u32::MAX),
    })
}

#[cfg(test)]
mod tests {
    use crate::test_support::TestRepo;

    #[test]
    fn lists_commits_newest_first() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "the first message");
        let second = test.commit(&[("a.txt", "two")], "the second message");
        let third = test.commit(&[("a.txt", "three")], "the third message");
        let repo = test.open();

        let commits = repo.log(0, 10).expect("the log");

        let ids: Vec<&str> = commits.iter().map(|commit| commit.id.as_str()).collect();
        assert_eq!(ids, vec![third.as_str(), second.as_str(), first.as_str()]);
        assert_eq!(commits[0].subject, "the third message");
        assert_eq!(commits[0].short_id, third[..7]);
        assert_eq!(commits[0].author, "Blastr Test");
        assert_eq!(commits[0].parents, 1);
        assert_eq!(commits[2].subject, "the first message");
        assert_eq!(commits[2].parents, 0);
    }

    #[test]
    fn skip_and_limit_apply_to_the_newest_commits() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");
        test.commit(&[("a.txt", "three")], "third");
        let repo = test.open();

        let skipped = repo.log(1, 1).expect("the log");
        let older = repo.log(2, 1).expect("the log");
        let unlimited = repo.log(0, 0).expect("the log");

        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].id, second);
        assert_eq!(older.len(), 1);
        assert_eq!(older[0].id, first);
        assert_eq!(unlimited.len(), 3);
    }

    #[test]
    fn an_empty_repository_has_no_commits() {
        let test = TestRepo::new();

        let commits = test.open().log(0, 10).expect("the log");

        assert!(commits.is_empty());
    }
}
