# Mergify-cli

## What are stacked pull requests

Imagine you have a branch `feature` with three commit `A`, `B` and `C` based on branch `main`.

To create stacked pull requests with this feature branch you have to:

* Creates a branch `feature-A` with only commit `A` based on `main` and create a pull request
* Creates a branch `feature-B` with only commit `B` based on `feature-A` and create a pull request
* Creates a branch `feature-C` with only commit `C` based on `feature-C` and create a pull request

And of courses stacked pull requests workflow doesn't work with forked
repository as base branch of a pull request must live in the main repsository.

## The problem with GitHub stacked pull request

Then you change commit `A`, well you have to updated all your branches manually
and update each pull requests.

This a time consuming and error-prone process.

You can't update only `feature-A` otherwise other branch become outdated.


## The compromise for the automation of the tidy process

Each time you change the feature branch and rebase/update your sub-feature
branches all commit sha changes and maybe you added a commit, or removed one, or
even reordered them. Making impossible to track what going on with the commit index or sha.

Mergify-cli uses the same technique as other Code Review tools like [Gerrit](https://www.gerritcodereview.com/)
It injects automatically into commit message a random ID (via a git commit-msg hook), example:

```
Change-Id: I7074fdf5e24e2d4de721936260e4b962532c9735
```

These IDs will allow to track what is going on on your `feature` branch.

Like Gerrit, it makes no compromise and it assumes your commit messages are well
written and use them in title and body of pull requests.

Also Mergify-cli leverages the GitHub API to do the tidy jobs, keeping your
local git repository clean.

Unlike Gerrit, it can't use custom namespace to hide the git reference on the
remote repository, the branches must be in `heads` namespace to be able to open
pull requests.

To ensures stacked pull requests can't be merged until the dependencies are not ready.
We put the draft flag.

## How the git workflow changes:

To uses Mergify-cli you have to replace:

```bash
$ git checkout feature-A
...do some change and commit...
$ git push origin feature-A -f

$ git checkout feature-B
$ git rebase feature-A
$ git push origin feature-B -f

$ git checkout feature-C
$ git rebase feature-B
$ git push origin feature-C -f
```

by:

```bash
...do some change and commit...
$ mrgfy push-stack
```

## Setup the git commit-msg hook:

```bash
$ mrgfy push-stack -s
```

## Push Stack your pull request like a pro 🦾

```bash
$ mrgfy push-stack
```

Enjoy!

## Installation

```bash
curl https://github.com/Mergifyio/mrgfy/releases/download/....
chmod +x mrgfy
sudo mv mrgfy /usr/local/bin/mrgfy
```
