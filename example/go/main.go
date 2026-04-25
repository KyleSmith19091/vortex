package main

import (
	"fmt"
	"sync"
)

func init() {
	fmt.Println("Init run")
}

func main() {
	wg := sync.WaitGroup{}
	for i := range 4 {
		go func() {
			fmt.Printf("Thread: %d done\n", i)
			wg.Done()
		}()
		wg.Add(1)
	}

	wg.Wait()

	fmt.Println("Hello world")
}
