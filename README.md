hyper-client-pool
======

## Building the dev environment
Build and start up the docker compose environment:
```
./script/up
```

Start the tty in the hyper-client-pool container:
```
./script/console
```

Now you can run the tests, etc:
```
./script/test
```

Note: You _can not_ successfully run tests outside of the container. Some tests will hang, others fails.

You can tear down the dev environment with:
```
./script/down
```
