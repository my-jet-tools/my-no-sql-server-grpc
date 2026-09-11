use std::net::SocketAddr;

/// Where the two listeners bind unless the environment says otherwise.
///
/// `0.0.0.0` on both: a server in a container is reached from outside it, and a
/// loopback default would be a server nobody can talk to. Narrowing the gRPC
/// side to `127.0.0.1` is a deployment's decision - it is the write transport,
/// and there are deployments where nothing outside the host may reach it - so it
/// is what the variable is for rather than what the default assumes.
pub const DEFAULT_HTTP_ENDPOINT: &str = "0.0.0.0:8000";
pub const DEFAULT_GRPC_ENDPOINT: &str = "0.0.0.0:8888";

/// The variables that override them. Read once, at start up: a listener cannot
/// be moved after it is bound, so re-reading them later would only be a lie
/// about where the server is.
///
/// Both are named `..._ENDPOINT` because both take one: a value here is a host
/// and a port, and a name promising a port would be a name that lies in exactly
/// the deployment which narrows gRPC to loopback.
pub const HTTP_ENDPOINT_VAR: &str = "LISTEN_HTTP_ENDPOINT";
pub const GRPC_ENDPOINT_VAR: &str = "LISTEN_GRPC_ENDPOINT";

pub fn http() -> SocketAddr {
    resolve(HTTP_ENDPOINT_VAR, DEFAULT_HTTP_ENDPOINT)
}

pub fn grpc() -> SocketAddr {
    resolve(GRPC_ENDPOINT_VAR, DEFAULT_GRPC_ENDPOINT)
}

fn resolve(var_name: &str, default: &str) -> SocketAddr {
    let Ok(value) = std::env::var(var_name) else {
        return parse(default, default, var_name);
    };

    let value = value.trim();

    if value.is_empty() {
        return parse(default, default, var_name);
    }

    parse(value, default, var_name)
}

/// A value is an endpoint - `0.0.0.0:8000` - or a bare port, which is taken as
/// that port on `0.0.0.0`. The bare form is here because one of the two
/// variables is named after a port, and somebody who sets `8080` in it has said
/// something unambiguous.
fn parse(value: &str, default: &str, var_name: &str) -> SocketAddr {
    if let Ok(port) = value.parse::<u16>() {
        return SocketAddr::from(([0, 0, 0, 0], port));
    }

    match value.parse::<SocketAddr>() {
        Ok(addr) => addr,
        // A refusal to start, and not a fall back to the default: somebody who
        // wrote the variable meant to be somewhere else, and a server which
        // quietly came up on the default port would be found by nobody.
        Err(err) => panic!(
            "{var_name}='{value}' is neither an endpoint like '{default}' nor a port number: {err}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_ports_this_server_is_reached_on() {
        assert_eq!(
            parse(
                DEFAULT_HTTP_ENDPOINT,
                DEFAULT_HTTP_ENDPOINT,
                HTTP_ENDPOINT_VAR
            )
            .to_string(),
            "0.0.0.0:8000"
        );
        assert_eq!(
            parse(
                DEFAULT_GRPC_ENDPOINT,
                DEFAULT_GRPC_ENDPOINT,
                GRPC_ENDPOINT_VAR
            )
            .to_string(),
            "0.0.0.0:8888"
        );
    }

    #[test]
    fn an_endpoint_is_taken_as_written_host_included() {
        let addr = parse("127.0.0.1:8888", DEFAULT_GRPC_ENDPOINT, GRPC_ENDPOINT_VAR);

        assert_eq!(addr.to_string(), "127.0.0.1:8888");
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn a_bare_port_is_that_port_on_every_interface() {
        assert_eq!(
            parse("9000", DEFAULT_HTTP_ENDPOINT, HTTP_ENDPOINT_VAR).to_string(),
            "0.0.0.0:9000"
        );
    }

    #[test]
    #[should_panic(expected = "is neither an endpoint")]
    fn a_value_that_says_nothing_stops_the_server() {
        parse("somewhere-else", DEFAULT_HTTP_ENDPOINT, HTTP_ENDPOINT_VAR);
    }

    /// An empty variable is a variable somebody left behind - `FOO=` in a
    /// compose file - and it must read as "not set" rather than as an error.
    #[test]
    fn an_empty_variable_is_not_set() {
        unsafe { std::env::set_var("LISTEN_ENDPOINTS_TEST_EMPTY", "  ") };

        assert_eq!(
            resolve("LISTEN_ENDPOINTS_TEST_EMPTY", DEFAULT_HTTP_ENDPOINT).to_string(),
            "0.0.0.0:8000"
        );

        unsafe { std::env::remove_var("LISTEN_ENDPOINTS_TEST_EMPTY") };
    }
}
