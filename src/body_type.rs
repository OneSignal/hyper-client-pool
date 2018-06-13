use hyper::{self, Body};
use futures::{Future, future, Stream};

pub trait ResponseBodyType {
    fn wrap(body: Body) -> Box<Future<Item=Self, Error=hyper::Error> + Send>;
}

impl ResponseBodyType for () {
    fn wrap(body: Body) -> Box<Future<Item=Self, Error=hyper::Error> + Send> {
        // Note that you must consume the body if you want keepalive
        // to take affect. Otherwise the connection fails with "unread body"
        // and closes.
        Box::new(
            body
            .skip_while(|_| future::ok(true))
            .collect()
            .map(|_| ())
        )
    }
}

impl ResponseBodyType for Vec<u8> {
    fn wrap(body: Body) -> Box<Future<Item=Self, Error=hyper::Error> + Send> {
        Box::new(
            body
            .fold(Vec::new(), |mut acc, chunk| {
                acc.extend_from_slice(&*chunk);
                future::ok::<_, hyper::Error>(acc)
            })
        )
    }
}
