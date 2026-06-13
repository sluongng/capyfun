package greeter

// Client talks to the greeter service.
type Client struct{ addr string }

// Connect dials addr and returns a Client. (v1 API)
func Connect(addr string) *Client { return &Client{addr: addr} }

// Addr reports the address the client targets.
func (c *Client) Addr() string { return c.addr }
