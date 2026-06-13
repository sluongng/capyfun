// A tiny program that uses three GitHub dependencies, so the demo has a real
// go.mod for `capyfun gen-go` to read.
package main

import (
	"fmt"

	"github.com/google/uuid"
	"github.com/pkg/errors"
	"github.com/spf13/pflag"
)

func main() {
	name := pflag.String("name", "world", "who to greet")
	pflag.Parse()

	id := uuid.New()
	if *name == "" {
		err := errors.New("name must not be empty")
		fmt.Println("error:", err)
		return
	}
	fmt.Printf("hello %s (request %s)\n", *name, id)
}
