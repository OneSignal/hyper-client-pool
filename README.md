hyper-client-pool
======

## Building the dev environment
Build the docker image for hyper-client-pool:
```
docker-compose build
```

Start the docker compose environment:
```
docker-compose up -d
```

Start the tty in the hyper-client-pool container:
```
docker-compose exec hyper-client-pool bash
```

Now you can run the tests, etc:
```
cargo test
```

You can tear down the dev environment with:
```
docker-compose down
```
