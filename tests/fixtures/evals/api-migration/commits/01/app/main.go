package main

import "acme/greeter"

func main() {
	c := greeter.Connect("localhost:9000")
	_ = c.Addr()
}
