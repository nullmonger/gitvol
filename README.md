## Git-backed Volume Driver for Docker.

Automatically clones a Git repository into the volume on mount and removes it on unmount.

### Installation

```bash
docker plugin install nerjs/gitvol
```

### Usage

Create a volume by specifying a repository URL and (optionally) a tag or branch:

```bash
docker volume create -d nerjs/gitvol \
  -o url=https://github.com/username/repository.git \
  -o tag=v1.0.0 \
  my-repo
```
Run a container with the volume:
```bash
docker run --rm -it -v my-repo:/data alpine ls -la /data
```
Remove the volume:
```bash
docker volume rm my-repo
```

---

### docker-compose / Swarm example

```yaml
version: "3.8"

services:
  app:
    image: alpine
    command: ["ls", "-la", "/data"]
    volumes:
      - my-repo:/data

volumes:
  my-repo:
    driver: nerjs/gitvol
    driver_opts:
      url: https://github.com/nullmonger/gitvol.git
      # Choose ONE of the two lines below (or omit both). Tags are recommended.
      # tag: v1.0.0
      # branch: main
```

--- 

## Options

- `url` (required) — Git-compatible repository URL. See [Git URLs](https://git-scm.com/docs/git-clone#_git_urls) (now supported only http(s)).

- `tag` (optional) — checkout a specific tag (__recommended__).

- `branch` (optional) — checkout a branch. **Not recommended** since branch contents may change between mounts.

- `refetch` (optional, default `"false"`) — when set to `"true"`, the plugin runs `git fetch` on each mount attempt, so the repository is updated if there are changes upstream.

> `tag` and `branch` are **mutually exclusive**.

### How it works

- The repository is cloned once per ***volume*** (unique per volume name, but not per container).

- Multiple containers can share the same volume — they all see the same underlying clone.

```yaml
version: '3'
services:
    static1:
        volumes:
            - 'my-vol:/srv/http'
        ports:
            - '8080:8043'
        image: 'pierrezemb/gostatic'
    static2:
        volumes:
            - 'my-vol:/srv/http'
        ports:
            - '8081:8043'
        image: 'pierrezemb/gostatic'
        
        
volumes:
  my-vol:
    driver: nerjs/gitvol
    driver_opts:
      url: https://github.com/nerjs/gitvol-test.git
      refetch: "true"

```

Both `static1` and `static2` mount the same volume. With `refetch: "true"`, restarting one container (e.g. `docker compose restart static1`) triggers a `git fetch` in the volume, so both containers see updated repository contents.

---

## Private repositories

Currently supported via embedding credentials into the URL, e.g.:

```bash
docker volume create -d nerjs/gitvol \
  -o url=https://<github_pat_token>@github.com/nullmonger/gitvol.git \
  my-private-repo
```