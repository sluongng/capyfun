package greeter

// Client talks to the greeter service.
type Client struct{ addr string }

// New dials addr and returns a Client. (v2 API; replaces Connect)
func New(addr string) *Client { return &Client{addr: addr} }

// Addr reports the address the client targets.
func (c *Client) Addr() string { return c.addr }
