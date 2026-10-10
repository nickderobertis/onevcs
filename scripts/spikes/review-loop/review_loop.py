#!/usr/bin/env python3
"""Spike harness: measure the review loop's comment reads and merge restack on real GitHub.

A throwaway measurement for the plan `authoring:pr-feedback-rework`, kept on this
spike's preserved branch and never landed. It drives `gh` and `git` directly — it is
not onevcs code — against a disposable repository whose name must end in `-smoke`:

    scripts/spikes/review-loop/review_loop.py --repo OWNER/NAME-smoke [--out DIR]

(or `REVIEW_LOOP_REPO=OWNER/NAME-smoke`). One run builds a stack of draft pull
requests, posts review feedback, reads every pull request's comments through each
candidate transport (recording calls, allowance cost, bytes and wall time), replies,
restacks by merge, opens a cold-host onevcs session on a child, and then closes every
pull request and deletes every branch it pushed. Everything it measured is written
to `results.json` and `summary.md` under `--out`; nothing is written to this checkout.

The token is shared with every other consumer on the host, so a per-call delta of
`X-RateLimit-Used` can include somebody else's calls. Each call's delta is recorded
as observed, and the 304 question is answered by a dedicated probe that compares a
burst of conditional reads with a burst of unconditional ones.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

MARKER = "<!-- onevcs:reply in-reply-to={id} -->"
PRS = ("A", "B", "C", "D")
REST_ENDPOINTS = (
    ("review-comments", "pulls/{n}/comments"),
    ("issue-comments", "issues/{n}/comments"),
    ("reviews", "pulls/{n}/reviews"),
)
PROBE_BURST = 20

PR_FIELDS = """
fragment Feedback on PullRequest {
  number isDraft updatedAt
  reviewThreads(first: 50) { totalCount nodes {
    id isResolved isOutdated path line originalLine diffSide
    comments(first: 50) { totalCount nodes {
      id databaseId author { login } authorAssociation body createdAt updatedAt
      lastEditedAt replyTo { databaseId } viewerDidAuthor } } } }
  comments(first: 100) { totalCount nodes {
    id databaseId author { login } authorAssociation body createdAt updatedAt
    lastEditedAt viewerDidAuthor } }
  reviews(first: 50) { totalCount nodes {
    id databaseId state body author { login } createdAt updatedAt lastEditedAt
    viewerDidAuthor } }
}
"""

ONE_PR_QUERY = (
    """
query($owner: String!, $name: String!, $number: Int!) {
  rateLimit { cost remaining used resetAt }
  repository(owner: $owner, name: $name) { pullRequest(number: $number) { ...Feedback } }
}
"""
    + PR_FIELDS
)


def batched_query(numbers: dict[str, int]) -> str:
    """One GraphQL query reading every pull request of the stack through aliases."""
    aliases = "\n".join(
        f"    pr{key}: pullRequest(number: {number}) {{ ...Feedback }}"
        for key, number in numbers.items()
    )
    return (
        "query($owner: String!, $name: String!) {\n"
        "  rateLimit { cost remaining used resetAt }\n"
        "  repository(owner: $owner, name: $name) {\n"
        f"{aliases}\n"
        "  }\n}\n" + PR_FIELDS
    )


class HarnessError(RuntimeError):
    """A step the measurement depends on failed; the message names it."""


@dataclass
class Response:
    status: int
    headers: dict[str, str]
    body: str
    seconds: float

    def json(self) -> Any:
        return json.loads(self.body) if self.body.strip() else None


@dataclass
class Ledger:
    """Every GitHub call the harness made, with its allowance deltas.

    One token's calls were observed reported against more than one window of the
    same resource (distinct `X-RateLimit-Reset` values), so a delta is taken against
    the previous call reported in the *same* window, never across two.
    """

    calls: list[dict[str, Any]] = field(default_factory=list)
    last_used: dict[tuple[str, int], int] = field(default_factory=dict)

    def delta(self, headers: dict[str, str]) -> tuple[str | None, int | None]:
        resource = headers.get("x-ratelimit-resource")
        if resource is None or "x-ratelimit-used" not in headers:
            return resource, None
        used = int(headers["x-ratelimit-used"])
        window = (resource, int(headers.get("x-ratelimit-reset", "0")))
        previous = self.last_used.get(window)
        self.last_used[window] = used
        return resource, None if previous is None else used - previous

    def windows(self) -> list[dict[str, Any]]:
        """Each (resource, reset) window the calls were reported against."""
        seen: dict[tuple[str, int], list[int]] = {}
        for call in self.calls:
            if call["resource"] and call["used"] >= 0:
                seen.setdefault((call["resource"], call["reset"]), []).append(
                    call["used"]
                )
        return [
            {
                "resource": r,
                "reset": reset,
                "calls": len(u),
                "used_min": min(u),
                "used_max": max(u),
            }
            for (r, reset), u in sorted(seen.items())
        ]


class GitHub:
    """`gh api` with headers, timing and allowance accounting on every call."""

    def __init__(self, repo: str, ledger: Ledger) -> None:
        self.repo = repo
        self.owner, self.name = repo.split("/", 1)
        self.ledger = ledger

    def call(
        self,
        method: str,
        path: str,
        *,
        phase: str,
        label: str,
        pr: str | None = None,
        body: Any = None,
        headers: tuple[str, ...] = (),
        allow: tuple[int, ...] = (),
    ) -> Response:
        args = [
            "gh",
            "api",
            "-i",
            "-X",
            method,
            path,
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
        ]
        for header in headers:
            args += ["-H", header]
        stdin = None
        if body is not None:
            args += ["--input", "-"]
            stdin = json.dumps(body)
        started = time.perf_counter()
        proc = subprocess.run(  # noqa: S603 - argv is built here, never by a shell
            args, input=stdin, capture_output=True, text=True, check=False
        )
        seconds = time.perf_counter() - started
        response = parse_include(proc.stdout, seconds)
        resource, delta = self.ledger.delta(response.headers)
        record: dict[str, Any] = {
            "phase": phase,
            "label": label,
            "pr": pr,
            "method": method,
            "path": path if path == "graphql" else path.split("?")[0],
            "status": response.status,
            "seconds": round(seconds, 4),
            "bytes": len(response.body.encode()),
            "resource": resource,
            "used": int(response.headers.get("x-ratelimit-used", "-1")),
            "remaining": int(response.headers.get("x-ratelimit-remaining", "-1")),
            "reset": int(response.headers.get("x-ratelimit-reset", "-1")),
            "used_delta": delta,
            "conditional": any(h.lower().startswith("if-none-match") for h in headers),
        }
        if path == "graphql":
            payload = response.json() or {}
            rate = (payload.get("data") or {}).get("rateLimit") or {}
            record["graphql_cost"] = rate.get("cost")
            if payload.get("errors"):
                record["graphql_errors"] = payload["errors"]
        self.ledger.calls.append(record)
        if response.status >= 400 and response.status not in allow:
            raise HarnessError(
                f"{method} {path} answered {response.status}: {response.body[:400]}"
                f" {proc.stderr.strip()}"
            )
        return response

    def rest(self, method: str, path: str, **kwargs: Any) -> Response:
        return self.call(method, f"repos/{self.repo}/{path}", **kwargs)

    def graphql(self, query: str, variables: dict[str, Any], **kwargs: Any) -> Response:
        return self.call(
            "POST", "graphql", body={"query": query, "variables": variables}, **kwargs
        )

    def meter(self, phase: str) -> dict[str, dict[str, int]]:
        """Read each bucket off a charged call's own headers (costs 1 core, 1 point).

        `GET /rate_limit` was observed answering a different window from the one the
        calls are charged against, so the buckets are read where the charge is made.
        """
        core = self.call("GET", f"repos/{self.repo}", phase=phase, label="meter-core")
        gql = self.graphql(
            "query { rateLimit { cost used remaining resetAt } }",
            {},
            phase=phase,
            label="meter-graphql",
        )
        return {
            name: {
                "used": int(r.headers["x-ratelimit-used"]),
                "reset": int(r.headers["x-ratelimit-reset"]),
            }
            for name, r in (("core", core), ("graphql", gql))
        }

    def rate_limit_endpoint(self) -> dict[str, Any]:
        """`GET /rate_limit`'s own answer, kept only to compare with the headers."""
        proc = subprocess.run(  # noqa: S603
            ["gh", "api", "rate_limit"], capture_output=True, text=True, check=True
        )
        resources = json.loads(proc.stdout)["resources"]
        return {key: resources[key] for key in ("core", "graphql")}


def spent(
    before: dict[str, dict[str, int]], after: dict[str, dict[str, int]]
) -> dict[str, int | None]:
    """Bucket movement between two meter readings; None where the window reset."""
    return {
        k: after[k]["used"] - before[k]["used"]
        if after[k]["reset"] == before[k]["reset"]
        else None
        for k in ("core", "graphql")
    }


def parse_include(stdout: str, seconds: float) -> Response:
    """Split `gh api -i` output into status, lower-cased headers and body."""
    parts = re.split(r"(\r?\n\r?\n)", stdout, maxsplit=1)
    head = parts[0]
    body = parts[2] if len(parts) == 3 else ""
    lines = head.splitlines()
    status = 0
    if lines and lines[0].startswith("HTTP/"):
        status = int(lines[0].split()[1])
    headers: dict[str, str] = {}
    for line in lines[1:]:
        key, sep, value = line.partition(":")
        if sep:
            headers[key.strip().lower()] = value.strip()
    return Response(status=status, headers=headers, body=body, seconds=seconds)


class Git:
    """Real git in one clone, under a git configuration private to the run."""

    def __init__(self, checkout: Path, env: dict[str, str]) -> None:
        self.checkout = checkout
        self.env = env

    def __call__(self, *args: str) -> str:
        proc = subprocess.run(  # noqa: S603
            ["git", *args],
            cwd=self.checkout,
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode != 0:
            raise HarnessError(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
        return proc.stdout.strip()


def write_lines(path: Path, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(f"{line}\n" for line in lines))


class Run:
    def __init__(self, repo: str, out: Path, work: Path) -> None:
        self.repo = repo
        self.out = out
        self.work = work
        self.run_id = (
            datetime.now(timezone.utc).strftime("%Y%m%d%H%M%S")
            + "-"
            + secrets.token_hex(2)
        )
        self.prefix = f"spike-review-loop/{self.run_id}"
        self.dir = f"spike-review-loop/{self.run_id}"
        self.ledger = Ledger()
        self.gh = GitHub(repo, self.ledger)
        self.gitconfig = work / "gitconfig"
        self.gitconfig.write_text(
            "[user]\n\tname = onevcs review-loop spike\n\temail = spike@example.invalid\n"
            "[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n"
            "[advice]\n\tdetachedHead = false\n"
            '[credential "https://github.com"]\n\thelper = !gh auth git-credential\n'
        )
        self.env = {**os.environ, "GIT_CONFIG_GLOBAL": str(self.gitconfig)}
        self.env.pop("ONEVCS_SESSION", None)
        self.checkout = work / "checkout"
        self.git = Git(self.checkout, self.env)
        self.branches: dict[str, str] = {
            k: f"{self.prefix}/{k}" for k in ("A", "B", "C", "S", "D")
        }
        self.pushed: list[str] = []
        self.numbers: dict[str, int] = {}
        self.node_ids: dict[str, str] = {}
        self.etags: dict[tuple[str, str], str] = {}
        self.rounds: list[dict[str, Any]] = []
        self.facts: dict[str, Any] = {}
        self.payloads: dict[str, dict[str, Any]] = {}
        self.started = time.perf_counter()

    # ---------------------------------------------------------------- step 1
    def build_stack(self) -> None:
        subprocess.run(  # noqa: S603
            [
                "git",
                "clone",
                "-q",
                f"https://github.com/{self.repo}.git",
                str(self.checkout),
            ],
            env=self.env,
            check=True,
            capture_output=True,
        )
        git = self.git
        root = self.checkout / self.dir

        def commit_file(branch: str, start: str, name: str, lines: list[str]) -> None:
            git("checkout", "-q", "-B", self.branches[branch], start)
            write_lines(root / name, lines)
            git("add", "-A")
            git("commit", "-q", "-m", f"spike: {branch} adds {name}")

        commit_file("A", "origin/main", "a.txt", [f"A line {i}" for i in range(1, 6)])
        self.push("A")
        commit_file(
            "B", self.branches["A"], "b.txt", [f"B line {i}" for i in range(1, 8)]
        )
        self.push("B")
        commit_file("C", "origin/main", "c.txt", [f"C line {i}" for i in range(1, 6)])
        self.push("C")
        git("checkout", "-q", "-B", self.branches["S"], "origin/main")
        git(
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "spike: S merges A",
            f"origin/{self.branches['A']}",
        )
        git(
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "spike: S merges C",
            f"origin/{self.branches['C']}",
        )
        self.push("S")
        commit_file(
            "D", self.branches["S"], "d.txt", [f"D line {i}" for i in range(1, 6)]
        )
        self.push("D")
        bases = {
            "A": "main",
            "B": self.branches["A"],
            "C": "main",
            "D": self.branches["S"],
        }
        for key in PRS:
            pr = self.gh.rest(
                "POST",
                "pulls",
                phase="stack",
                label="open-draft",
                pr=key,
                body={
                    "title": f"DO NOT MERGE: review-loop spike {self.run_id} {key}",
                    "head": self.branches[key],
                    "base": bases[key],
                    "body": f"Throwaway pull request of the review-loop spike run {self.run_id}.",
                    "draft": True,
                },
            ).json()
            self.numbers[key] = pr["number"]
            self.node_ids[key] = pr["node_id"]
            if not pr["draft"]:
                raise HarnessError(f"{key} was not opened as a draft")
        self.facts["pull_requests"] = {
            k: {"number": n, "head": self.branches[k], "base": bases[k]}
            for k, n in self.numbers.items()
        }
        self.facts["synthetic_base"] = {
            "branch": self.branches["S"],
            "merged": ["A", "C"],
        }

    def push(self, key: str) -> str:
        self.git("push", "-q", "origin", f"{self.branches[key]}:{self.branches[key]}")
        if self.branches[key] not in self.pushed:
            self.pushed.append(self.branches[key])
        return self.git("rev-parse", self.branches[key])

    # ---------------------------------------------------------------- step 2
    def post_feedback(self) -> None:
        n_b, n_d = self.numbers["B"], self.numbers["D"]
        head_b = self.git("rev-parse", self.branches["B"])
        line = self.gh.rest(
            "POST",
            f"pulls/{n_b}/comments",
            phase="feedback",
            label="line-comment",
            pr="B",
            body={
                "body": "Reviewer: line 3 should say why.",
                "commit_id": head_b,
                "path": f"{self.dir}/b.txt",
                "line": 3,
                "side": "RIGHT",
            },
        ).json()
        review = self.gh.rest(
            "POST",
            f"pulls/{n_b}/reviews",
            phase="feedback",
            label="review-summary",
            pr="B",
            body={
                "body": "Reviewer summary: the B change needs a rationale.",
                "event": "COMMENT",
            },
        ).json()
        convo = self.gh.rest(
            "POST",
            f"issues/{n_d}/comments",
            phase="feedback",
            label="conversation-comment",
            pr="D",
            body={"body": "Reviewer: D should mention the synthetic base."},
        ).json()
        self.facts["feedback"] = {
            "line_comment_id": line["id"],
            "line_comment_review_id": line.get("pull_request_review_id"),
            "review_id": review["id"],
            "review_node_id": review["node_id"],
            "review_state": review["state"],
            "conversation_comment_id": convo["id"],
            "conversation_comment_url": convo["html_url"],
        }

    def edit_comment(self) -> None:
        cid = self.facts["feedback"]["line_comment_id"]
        edited = "Reviewer (edited): line 3 should say why, and cite the plan."
        self.gh.rest(
            "PATCH",
            f"pulls/comments/{cid}",
            phase="feedback",
            label="edit-line-comment",
            pr="B",
            body={"body": edited},
        )
        self.facts["edit"] = {"comment_id": cid, "new_body": edited}

    # ---------------------------------------------------------------- step 3
    def read_round(self, name: str, changed: list[str]) -> None:
        """Read every pull request through every transport; `changed` names what moved."""
        phase = f"read:{name}"
        entry: dict[str, Any] = {
            "round": name,
            "changed_since_previous": changed,
            "reads": [],
        }
        for key in PRS:
            n = self.numbers[key]
            for transport in ("rest-plain", "rest-conditional"):
                for endpoint, template in REST_ENDPOINTS:
                    headers: tuple[str, ...] = ()
                    etag = self.etags.get((key, endpoint))
                    if transport == "rest-conditional" and etag:
                        headers = (f"If-None-Match: {etag}",)
                    resp = self.gh.rest(
                        "GET",
                        template.format(n=n) + "?per_page=100",
                        phase=phase,
                        label=f"{transport}:{endpoint}",
                        pr=key,
                        headers=headers,
                    )
                    if "link" in resp.headers and 'rel="next"' in resp.headers["link"]:
                        raise HarnessError(
                            f"{endpoint} of {key} paginated; the spike assumes one page"
                        )
                    if transport == "rest-conditional" and resp.status == 200:
                        self.etags[(key, endpoint)] = resp.headers.get("etag", "")
                    if resp.status == 200:
                        self.payloads.setdefault(name, {})[f"{key}:{endpoint}"] = (
                            resp.json()
                        )
            resp = self.gh.graphql(
                ONE_PR_QUERY,
                {"owner": self.gh.owner, "name": self.gh.name, "number": n},
                phase=phase,
                label="graphql-per-pr",
                pr=key,
            )
            self.payloads.setdefault(name, {})[f"{key}:graphql"] = resp.json()["data"][
                "repository"
            ]["pullRequest"]
        self.gh.graphql(
            batched_query(self.numbers),
            {"owner": self.gh.owner, "name": self.gh.name},
            phase=phase,
            label="graphql-batched",
            pr="ALL",
        )
        entry["reads"] = [c for c in self.ledger.calls if c["phase"] == phase]
        self.rounds.append(entry)

    def probe_304(self) -> None:
        """A burst of conditional reads against a burst of unconditional ones."""
        key, endpoint, template = "B", "review-comments", "pulls/{n}/comments"
        path = template.format(n=self.numbers[key]) + "?per_page=100"
        etag = self.etags[(key, endpoint)]
        result: dict[str, Any] = {
            "burst": PROBE_BURST,
            "endpoint": f"GET /repos/{{o}}/{{r}}/{template}",
        }
        for kind, headers in (
            ("conditional", (f"If-None-Match: {etag}",)),
            ("unconditional", ()),
        ):
            before = self.gh.meter(f"meter:probe-304:{kind}")["core"]
            phase = f"probe-304:{kind}"
            for _ in range(PROBE_BURST):
                self.gh.rest(
                    "GET", path, phase=phase, label=kind, pr=key, headers=headers
                )
            after = self.gh.meter(f"meter:probe-304:{kind}")["core"]
            calls = [c for c in self.ledger.calls if c["phase"] == phase]
            result[kind] = {
                "statuses": sorted({c["status"] for c in calls}),
                "core_used_before": before["used"],
                "core_used_after": after["used"],
                # The closing meter's own core read is the one charge not the burst's.
                "core_used_delta_excluding_meter": after["used"] - before["used"] - 1
                if after["reset"] == before["reset"]
                else None,
                "per_call_header_deltas": [c["used_delta"] for c in calls],
                "zero_delta_calls": sum(1 for c in calls if c["used_delta"] == 0),
                "median_seconds": round(
                    statistics.median(c["seconds"] for c in calls), 4
                ),
            }
        self.facts["probe_304"] = result

    # ---------------------------------------------------------------- step 4
    def reply(self) -> None:
        fb = self.facts["feedback"]
        n_b, n_d = self.numbers["B"], self.numbers["D"]
        thread_reply = self.gh.rest(
            "POST",
            f"pulls/{n_b}/comments/{fb['line_comment_id']}/replies",
            phase="reply",
            label="thread-reply",
            pr="B",
            body={
                "body": "Done: line 3 now says why.\n\n"
                + MARKER.format(id=fb["line_comment_id"])
            },
        ).json()
        convo_reply = self.gh.rest(
            "POST",
            f"issues/{n_d}/comments",
            phase="reply",
            label="conversation-reply",
            pr="D",
            body={
                "body": f"Re: {fb['conversation_comment_url']}\n\nDone: D now mentions S.\n\n"
                + MARKER.format(id=fb["conversation_comment_id"])
            },
        ).json()
        attempts: dict[str, Any] = {}
        resp = self.gh.rest(
            "POST",
            f"pulls/{n_b}/comments/{fb['review_id']}/replies",
            phase="reply",
            label="review-summary-reply:rest-replies",
            pr="B",
            body={
                "body": "Reply to the summary.\n\n" + MARKER.format(id=fb["review_id"])
            },
            allow=(404, 422),
        )
        attempts["rest_replies_endpoint_with_review_id"] = {
            "status": resp.status,
            "body": resp.json(),
        }
        resp = self.gh.rest(
            "POST",
            f"pulls/{n_b}/comments",
            phase="reply",
            label="review-summary-reply:rest-in-reply-to",
            pr="B",
            body={
                "body": "Reply to the summary.\n\n" + MARKER.format(id=fb["review_id"]),
                "in_reply_to": fb["review_id"],
            },
            allow=(404, 422),
        )
        attempts["rest_in_reply_to_review_id"] = {
            "status": resp.status,
            "body": resp.json(),
        }
        resp = self.gh.graphql(
            "mutation($thread: ID!, $body: String!) { addPullRequestReviewThreadReply("
            "input: {pullRequestReviewThreadId: $thread, body: $body}) { comment { id } } }",
            {"thread": fb["review_node_id"], "body": "Reply to the summary."},
            phase="reply",
            label="review-summary-reply:graphql-thread-reply",
            pr="B",
        )
        attempts["graphql_thread_reply_with_review_node_id"] = {
            "status": resp.status,
            "body": resp.json(),
        }
        self.facts["review_summary_reply_attempts"] = attempts
        self.facts["replies"] = {
            "thread_reply_id": thread_reply["id"],
            "thread_reply_in_reply_to_id": thread_reply.get("in_reply_to_id"),
            "conversation_reply_id": convo_reply["id"],
        }

    def resolve_thread(self) -> None:
        thread = self.thread_of_line_comment("after-replies")
        resp = self.gh.graphql(
            "mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) { thread { isResolved } } }",
            {"id": thread["id"]},
            phase="resolve",
            label="resolve-thread",
            pr="B",
        )
        self.facts["resolve"] = {"thread_id": thread["id"], "answer": resp.json()}

    def thread_of_line_comment(self, round_name: str) -> dict[str, Any]:
        cid = self.facts["feedback"]["line_comment_id"]
        for thread in self.payloads[round_name]["B:graphql"]["reviewThreads"]["nodes"]:
            if any(c["databaseId"] == cid for c in thread["comments"]["nodes"]):
                return thread
        raise HarnessError(
            f"no review thread holds comment {cid} in round {round_name}"
        )

    # ---------------------------------------------------------------- step 5
    def files(self, label: str) -> dict[str, list[str]]:
        listing = {}
        for key in PRS:
            body = self.gh.rest(
                "GET",
                f"pulls/{self.numbers[key]}/files?per_page=100",
                phase="restack",
                label=f"files-{label}",
                pr=key,
            ).json()
            listing[key] = sorted(f["filename"] for f in body)
        return listing

    def wait_for_head(self, key: str, sha: str) -> None:
        for _ in range(30):
            pr = self.gh.rest(
                "GET",
                f"pulls/{self.numbers[key]}",
                phase="restack",
                label="await-head",
                pr=key,
            ).json()
            if pr["head"]["sha"] == sha:
                return
            time.sleep(2)
        raise HarnessError(f"pull request {key} never showed head {sha}")

    def restack(self) -> None:
        git = self.git
        before = self.files("before")
        git("checkout", "-q", self.branches["A"])
        write_lines(
            self.checkout / self.dir / "a.txt",
            [f"A line {i}" for i in range(1, 6)] + ["A line 6 (new)"],
        )
        git("commit", "-q", "-am", "spike: A gains a commit")
        heads = {"A": self.push("A")}
        git("fetch", "-q", "origin")
        git("checkout", "-q", self.branches["B"])
        git(
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "spike: restack B onto new A",
            f"origin/{self.branches['A']}",
        )
        heads["B"] = self.push("B")
        git("checkout", "-q", self.branches["S"])
        git(
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "spike: S takes new A",
            f"origin/{self.branches['A']}",
        )
        self.push("S")
        git("fetch", "-q", "origin")
        git("checkout", "-q", self.branches["D"])
        git(
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "spike: restack D onto new S",
            f"origin/{self.branches['S']}",
        )
        heads["D"] = self.push("D")
        for key, sha in heads.items():
            self.wait_for_head(key, sha)
        time.sleep(5)
        after = self.files("after")
        own = {k: [f"{self.dir}/{k.lower()}.txt"] for k in PRS}
        self.facts["restack"] = {
            "before": before,
            "after": after,
            "child_only_after": {k: after[k] == own[k] for k in PRS},
            "forced": False,
        }

    # ---------------------------------------------------------------- step 6
    def cold_host(self) -> None:
        home = self.work / "cold-onevcs-home"
        clone = self.work / "cold-checkout"
        env = {**self.env, "ONEVCS_HOME": str(home)}
        log: list[dict[str, Any]] = []

        def run(
            *argv: str, cwd: Path | None = None
        ) -> subprocess.CompletedProcess[str]:
            started = time.perf_counter()
            proc = subprocess.run(  # noqa: S603
                list(argv),
                cwd=cwd,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            log.append(
                {
                    "argv": list(argv),
                    "exit": proc.returncode,
                    "seconds": round(time.perf_counter() - started, 2),
                    "stdout": proc.stdout[-3000:],
                    "stderr": proc.stderr[-3000:],
                }
            )
            return proc

        before = self.gh.meter("meter:cold-host")
        version = run("onevcs", "--version").stdout.strip()
        if home.exists():
            raise HarnessError(f"the cold host's ONEVCS_HOME {home} already exists")
        run("git", "clone", "-q", f"https://github.com/{self.repo}.git", str(clone))
        if run("onevcs", "register", str(clone)).returncode != 0:
            raise HarnessError(f"onevcs register failed: {log[-1]['stderr']}")
        opened = run(
            "onevcs",
            "session",
            "open",
            str(clone),
            "--branch",
            self.branches["B"],
            "--base",
            self.branches["A"],
            "--pool",
            "0",
        )
        if opened.returncode != 0:
            raise HarnessError(f"onevcs session open failed: {opened.stderr}")
        session = json.loads(opened.stdout)
        worktree = Path(session["worktree"])
        write_lines(
            worktree / self.dir / "b-cold.txt", ["written by a cold-host session"]
        )
        run("git", "add", "-A", cwd=worktree)
        run(
            "git",
            "commit",
            "-q",
            "-m",
            "spike: cold-host session commits on B",
            cwd=worktree,
        )
        published = run("onevcs", "publish", session["token"], "--draft")
        shown = run("onevcs", "change", "show", session["token"])
        session_head = run("git", "rev-parse", "HEAD", cwd=worktree).stdout.strip()
        closed = run("onevcs", "session", "close", session["token"])
        after = self.gh.meter("meter:cold-host")
        open_prs = self.gh.rest(
            "GET",
            f"pulls?state=all&head={self.gh.owner}:{self.branches['B']}&per_page=100",
            phase="cold-host",
            label="list-prs-for-B",
            pr="B",
        ).json()
        pr_b = self.gh.rest(
            "GET",
            f"pulls/{self.numbers['B']}",
            phase="cold-host",
            label="read-B",
            pr="B",
        ).json()
        self.facts["cold_host"] = {
            "onevcs_version": version,
            "session": session,
            "publish_exit": published.returncode,
            "change_show_exit": shown.returncode,
            "session_close_exit": closed.returncode,
            "pull_requests_with_head_B": [
                {"number": p["number"], "state": p["state"], "draft": p["draft"]}
                for p in open_prs
            ],
            "adopted_existing": [p["number"] for p in open_prs] == [self.numbers["B"]],
            "B_still_draft": pr_b["draft"],
            "B_head_after": pr_b["head"]["sha"],
            "B_head_is_session_commit": pr_b["head"]["sha"] == session_head,
            "B_files_after": sorted(
                f["filename"]
                for f in self.gh.rest(
                    "GET",
                    f"pulls/{self.numbers['B']}/files?per_page=100",
                    phase="cold-host",
                    label="files-B",
                    pr="B",
                ).json()
            ),
            # Between the two meters only onevcs (and any other consumer of the
            # shared token) spent; the closing meter's own charge is subtracted.
            "allowance_spent_by_onevcs_and_others": {
                k: (v - 1 if v is not None else None)
                for k, v in spent(before, after).items()
            },
            "log": log,
        }

    # ---------------------------------------------------------------- step 7
    def cleanup(self) -> None:
        closed = {}
        for key, n in self.numbers.items():
            try:
                pr = self.gh.rest(
                    "PATCH",
                    f"pulls/{n}",
                    phase="cleanup",
                    label="close",
                    pr=key,
                    body={"state": "closed"},
                ).json()
                closed[key] = pr["state"]
            except HarnessError as error:
                closed[key] = f"failed: {error}"
        deleted = {}
        for branch in self.pushed:
            try:
                self.git("push", "-q", "origin", "--delete", branch)
                deleted[branch] = "deleted"
            except HarnessError as error:
                deleted[branch] = f"failed: {error}"
        remaining = (
            subprocess.run(  # noqa: S603
                [
                    "git",
                    "ls-remote",
                    "--heads",
                    "origin",
                    f"refs/heads/{self.prefix}/*",
                ],
                cwd=self.checkout,
                env=self.env,
                capture_output=True,
                text=True,
                check=False,
            ).stdout.strip()
            if self.checkout.exists()
            else ""
        )
        states = {}
        for key, n in self.numbers.items():
            states[key] = self.gh.rest(
                "GET", f"pulls/{n}", phase="cleanup", label="verify", pr=key
            ).json()["state"]
        self.facts["cleanup"] = {
            "closed": closed,
            "states_after": states,
            "deleted": deleted,
            "branches_left_on_origin": remaining.splitlines(),
        }


# -------------------------------------------------------------------- analysis
def summarise(run: Run) -> dict[str, Any]:
    calls = run.ledger.calls

    def per(transport: str, round_name: str, key: str) -> dict[str, Any]:
        sel = [
            c for c in calls if c["phase"] == f"read:{round_name}" and c["pr"] == key
        ]
        if transport == "graphql-per-pr":
            sel = [c for c in sel if c["label"] == "graphql-per-pr"]
        else:
            sel = [c for c in sel if c["label"].startswith(transport + ":")]
        return {
            "calls": len(sel),
            "statuses": [c["status"] for c in sel],
            "used_deltas": [c["used_delta"] for c in sel],
            "graphql_cost": [
                c.get("graphql_cost") for c in sel if c.get("graphql_cost") is not None
            ],
            "bytes": sum(c["bytes"] for c in sel),
            "seconds": [c["seconds"] for c in sel],
        }

    table = {
        r["round"]: {
            key: {
                t: per(t, r["round"], key)
                for t in ("rest-plain", "rest-conditional", "graphql-per-pr")
            }
            for key in PRS
        }
        | {
            "batched": [
                {
                    k: c.get(k)
                    for k in ("graphql_cost", "used_delta", "bytes", "seconds")
                }
                for c in calls
                if c["phase"] == f"read:{r['round']}"
                and c["label"] == "graphql-batched"
            ]
        }
        for r in run.rounds
    }

    capabilities = capability_check(run)
    by_resource: dict[str, int] = {}
    for c in calls:
        by_resource[c["resource"] or "none"] = (
            by_resource.get(c["resource"] or "none", 0) + 1
        )
    timing = {}
    for label in (
        "rest-plain",
        "rest-conditional",
        "graphql-per-pr",
        "graphql-batched",
    ):
        for status_kind in ("200", "304"):
            secs = [
                c["seconds"]
                for c in calls
                if c["phase"].startswith("read:")
                and c["label"].startswith(label)
                and str(c["status"]) == status_kind
            ]
            if secs:
                timing[f"{label}:{status_kind}"] = {
                    "n": len(secs),
                    "median": round(statistics.median(secs), 4),
                    "min": round(min(secs), 4),
                    "max": round(max(secs), 4),
                }
    return {
        "per_round": table,
        "capabilities": capabilities,
        "timing": timing,
        "calls_by_resource": by_resource,
        "calls_total": len(calls),
        "rate_limit_windows": run.ledger.windows(),
        "graphql_points_charged": sum(c.get("graphql_cost") or 0 for c in calls),
    }


def capability_check(run: Run) -> dict[str, Any]:
    """What each transport returned, decided from the payloads the run read."""
    fb, edit = run.facts["feedback"], run.facts["edit"]
    after_edit = run.payloads["after-edit"]
    after_replies = run.payloads["after-replies"]
    after_resolve = run.payloads.get("after-resolve", {})
    rest_comments = after_edit["B:review-comments"]
    rest_item = next(c for c in rest_comments if c["id"] == fb["line_comment_id"])
    keys = sorted(rest_item)
    gql = after_edit["B:graphql"]
    thread = next(
        t
        for t in gql["reviewThreads"]["nodes"]
        if any(c["databaseId"] == fb["line_comment_id"] for c in t["comments"]["nodes"])
    )
    gql_comment = next(
        c
        for c in thread["comments"]["nodes"]
        if c["databaseId"] == fb["line_comment_id"]
    )
    rest_replies = after_replies["B:review-comments"]
    reply_id = run.facts["replies"]["thread_reply_id"]
    rest_reply = next(c for c in rest_replies if c["id"] == reply_id)
    gql_threads = after_replies["B:graphql"]["reviewThreads"]["nodes"]
    gql_reply = next(
        c
        for t in gql_threads
        for c in t["comments"]["nodes"]
        if c["databaseId"] == reply_id
    )
    convo_id = run.facts["replies"]["conversation_reply_id"]
    rest_convo = next(
        c for c in after_replies["D:issue-comments"] if c["id"] == convo_id
    )
    gql_convo = next(
        c
        for c in after_replies["D:graphql"]["comments"]["nodes"]
        if c["databaseId"] == convo_id
    )
    original = next(c for c in rest_replies if c["id"] == fb["line_comment_id"])
    resolved_after_reply = next(
        t["isResolved"]
        for t in gql_threads
        if any(c["databaseId"] == reply_id for c in t["comments"]["nodes"])
    )
    resolve_round = after_resolve.get("B:graphql")
    resolved_after_mutation = None
    if resolve_round:
        resolved_after_mutation = next(
            t["isResolved"]
            for t in resolve_round["reviewThreads"]["nodes"]
            if any(c["databaseId"] == reply_id for c in t["comments"]["nodes"])
        )
    marker_reply = MARKER.format(id=fb["line_comment_id"])
    marker_convo = MARKER.format(id=fb["conversation_comment_id"])
    return {
        "rest": {
            "review_comment_keys": keys,
            "thread_id_field": [k for k in keys if "thread" in k],
            "outdated_flag_field": [k for k in keys if "outdated" in k],
            "resolved_flag_field": [k for k in keys if "resolv" in k],
            "edit_visible": rest_item["body"] == edit["new_body"]
            and rest_item["updated_at"] != rest_item["created_at"],
            "reply_links_parent_by": {
                "in_reply_to_id": rest_reply.get("in_reply_to_id")
            },
        },
        "graphql": {
            "thread_id": thread["id"],
            "isOutdated_present": "isOutdated" in thread,
            "isResolved_present": "isResolved" in thread,
            "edit_visible": gql_comment["body"] == edit["new_body"]
            and gql_comment["lastEditedAt"] is not None,
            "lastEditedAt": gql_comment["lastEditedAt"],
        },
        "marker_survives": {
            "rest_thread_reply": rest_reply["body"].endswith(marker_reply),
            "graphql_thread_reply": gql_reply["body"].endswith(marker_reply),
            "rest_conversation_reply": rest_convo["body"].endswith(marker_convo),
            "graphql_conversation_reply": gql_convo["body"].endswith(marker_convo),
        },
        "same_account_reply": {
            "reviewer_login": original["user"]["login"],
            "reply_login": rest_reply["user"]["login"],
            "reply_author_association": rest_reply.get("author_association"),
            "graphql_viewerDidAuthor": gql_reply["viewerDidAuthor"],
            "graphql_authorAssociation": gql_reply["authorAssociation"],
        },
        "thread_resolved_after_reply": resolved_after_reply,
        "thread_resolved_after_resolve_mutation": resolved_after_mutation,
    }


def render_summary(run: Run, results: dict[str, Any]) -> str:
    s = results["summary"]
    lines = [
        f"# review-loop spike run {run.run_id}",
        "",
        f"- repository: `{run.repo}`",
        f"- harness commit: `{results['harness']['commit']}` (tree clean: {results['harness']['clean']})",
        f"- wall time: {results['wall_seconds']}s; calls: {s['calls_total']} {s['calls_by_resource']}",
        f"- bucket movement over the run (headers; includes other consumers and onevcs):"
        f" {results['bucket_movement']}; graphql points the harness was charged:"
        f" {s['graphql_points_charged']}",
        f"- GET /rate_limit at start answered {results['rate_limit_endpoint_at_start']}"
        f" while the headers read {results['buckets_at_start']}",
        "",
        f"- rate-limit windows the calls were reported against: {s['rate_limit_windows']}",
        "",
        "## per-round reads (calls / used deltas / graphql cost / bytes)",
    ]
    for round_name, prs in s["per_round"].items():
        lines.append(f"### {round_name}")
        for key in PRS:
            for t, v in prs[key].items():
                lines.append(
                    f"- {key} {t}: {v['calls']} calls, statuses {v['statuses']}, deltas {v['used_deltas']},"
                    f" gql {v['graphql_cost']}, {v['bytes']} B, {v['seconds']} s"
                )
        lines.append(f"- batched: {prs['batched']}")
    lines += [
        "",
        "## timing",
        "```",
        json.dumps(s["timing"], indent=2),
        "```",
        "## capabilities",
        "```",
        json.dumps(s["capabilities"], indent=2),
        "```",
        "## facts",
        "```",
        json.dumps({k: v for k, v in run.facts.items() if k != "cold_host"}, indent=2),
        "```",
        "## cold host",
        "```",
        json.dumps(
            {k: v for k, v in run.facts.get("cold_host", {}).items() if k != "log"},
            indent=2,
        ),
        "```",
    ]
    return "\n".join(lines) + "\n"


def harness_commit() -> dict[str, Any]:
    here = Path(__file__).resolve().parent
    commit = subprocess.run(  # noqa: S603
        ["git", "rev-parse", "HEAD"],
        cwd=here,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    dirty = subprocess.run(  # noqa: S603
        ["git", "status", "--porcelain"],
        cwd=here,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    return {"commit": commit, "clean": not dirty}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--repo",
        default=os.environ.get("REVIEW_LOOP_REPO"),
        help="OWNER/NAME of a disposable repository ending in -smoke (or REVIEW_LOOP_REPO)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        help="directory for results.json and summary.md (default: a new temp dir)",
    )
    args = parser.parse_args()
    if not args.repo or not re.fullmatch(r"[\w.-]+/[\w.-]+-smoke", args.repo):
        parser.error(
            "--repo (or REVIEW_LOOP_REPO) must name OWNER/NAME where NAME ends in -smoke"
        )
    if shutil.which("gh") is None or shutil.which("onevcs") is None:
        parser.error("gh and onevcs must both be on PATH")
    out = args.out or Path(tempfile.mkdtemp(prefix="review-loop-"))
    out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="review-loop-work-"))
    harness = harness_commit()
    run = Run(args.repo, out, work)
    endpoint_answer = run.gh.rate_limit_endpoint()
    start = run.gh.meter("meter:run")
    failure = None
    try:
        run.build_stack()
        run.post_feedback()
        run.read_round("initial", ["everything: first read"])
        run.read_round("unchanged-1", [])
        run.edit_comment()
        run.read_round("after-edit", ["B: line comment edited"])
        run.reply()
        run.read_round("after-replies", ["B: thread reply", "D: conversation reply"])
        run.read_round("unchanged-2", [])
        run.probe_304()
        run.restack()
        run.read_round("after-restack", ["A, B, D: pushes only, no comment changed"])
        run.resolve_thread()
        run.read_round("after-resolve", ["B: thread resolved, no comment changed"])
        run.cold_host()
    except Exception as error:  # noqa: BLE001 - recorded, then cleanup still runs
        failure = f"{type(error).__name__}: {error}"
        print(f"review-loop: {failure}", file=sys.stderr)
    finally:
        run.cleanup()
    end = run.gh.meter("meter:run")
    results: dict[str, Any] = {
        "run_id": run.run_id,
        "repo": run.repo,
        "harness": harness,
        "failure": failure,
        "wall_seconds": round(time.perf_counter() - run.started, 1),
        "buckets_at_start": start,
        "buckets_at_end": end,
        # Includes every other consumer of the shared token over the run's span,
        # and the cold-host onevcs process's own calls, which the ledger never sees.
        "bucket_movement": spent(start, end),
        "rate_limit_endpoint_at_start": endpoint_answer,
        "facts": run.facts,
        "calls": run.ledger.calls,
        "payloads": run.payloads,
    }
    try:
        results["summary"] = summarise(run)
    except Exception as error:  # noqa: BLE001 - a failed run still writes what it has
        results["summary_error"] = f"{type(error).__name__}: {error}"
    (out / "results.json").write_text(json.dumps(results, indent=2, default=str))
    if "summary" in results:
        (out / "summary.md").write_text(render_summary(run, results))
    shutil.rmtree(work, ignore_errors=True)
    print(
        f"review-loop: {'FAILED' if failure else 'ok'} run {run.run_id}; results in {out}"
    )
    return 1 if failure or "summary_error" in results else 0


if __name__ == "__main__":
    sys.exit(main())
