# Scalpel Docker

This directory serves as documentation to setup your own Scalpel client using docker as the driver!
To get started, see the [Running Standalone](#running-standalone) or [Docker Compose](#docker-compose)
sections for the setup that you would like to run.

All docker images for this repository are published to the
[GitHub Container Registry](https://github.com/users/blockba5her/packages/container/package/scalpel).


## Running Standalone

This is how to run the Scalpel client with standalone docker, however it's probably easier to just
run it using docker compose instead.

- Make sure [Docker](https://www.docker.com/get-started) is installed on your system
- Open a terminal or command prompt
- Find the latest version at the [package](https://github.com/users/blockba5her/packages/container/scalpel/versions)
  page. **DON'T** use the `main` version, it might be unstable!
- Download the image using `docker pull ghcr.io/blockba5her/scalpel:<version>`, replacing `<version>`
  with the verion you want.
- `cd` to the directory you want to run the container in
- Copy the repo's [`settings.sample.yaml`](https://github.com/blockba5her/scalpel/blob/main/settings.sample.yaml)
  file to that directory as `settings.yaml` and edit
- Use one of the following commands to run:

```bash
# This will run the client in your terminal. You can stop using CTRL+C.
docker run -p 443:443 -v ./cache:/mangahome/cache -v ./settings.yaml:/mangahome/settings.yaml ghcr.io/blockba5her/scalpel:{version}

# This will run the client as a background process.
docker run -p 443:443 -v ./cache:/mangahome/cache -v ./settings.yaml:/mangahome/settings.yaml -d ghcr.io/blockba5her/scalpel:{version}
# To stop the client, use the following command to list all docker instances
docker ps
# Then copy the ID of the one running scalpel and run
docker stop {id}
```

## Docker Compose

This is how to run the Scalpel client with docker and docker compose.

- Make sure [Docker](https://www.docker.com/get-started) and
  [Docker Compose](https://docs.docker.com/compose/install/) are installed on your system
- Copy the [`docker-compose.yml`](https://github.com/blockba5her/scalpel/blob/main/docker/docker-compose.yml)
  file to an empty folder on your machine
- Change `{version}` in `docker-compose.yml` to the latest version on the
  [package](https://github.com/users/blockba5her/packages/container/scalpel/versions) page. **DON'T**
  use the `main` version, it might be unstable!
- Copy the repo's [`settings.sample.yaml`](https://github.com/blockba5her/scalpel/blob/main/settings.sample.yaml)
  file to that directory as `settings.yaml` and edit
- Use the following commands to run:

```bash
# This will start the docker compose instance in the background
docker-compose up -d

# To stop the client, use
docker-compose down
```


## Automatic Builds

Docker images for Scalpel are automatically built when new code is pushed to the `main` branch or
when a new version is released (new tag is created) for the repository. All docker images will be
published to `ghcr.io/blockba5her/scalpel:tag`. The different tags are listed below.

Please see the [package](https://github.com/users/blockba5her/packages/container/package/scalpel) page
for info on all versions currently released.

### Official Release Tags

Each official release (one marked by a git tag) will have its own version automatically built and
published in the container registry. The tags published for each release are as follows:

- `v<major>.<minor>.<revision>` (example: `v1.2.3`)
- `v<major>.<minor>` (example: `v1.2`)

Using these versions as compared to the below versions are recommended as these versions are
generally more stable and won't randomly become unstable.

### Unstable Tags

Unstable versions follow git branches as opposed to git tags. As of writing this, the only branch
that is published automatically is `main`, so all code pushed to the `main` branch will automatically
be built into the `main` docker tag.

These are considered unstable because not all new commits are guarenteed safe when pushed to the
repository, so it is not recommended to use the this docker tag when using the image.
