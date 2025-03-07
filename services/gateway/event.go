package gateway

import (
	"fmt"
	"sync"
)

// Event represents a generic event structure
type Event struct {
	Type string      `json:"type"` // Event type identifier
	Data interface{} `json:"data"` // Event payload data
}

// EventHandler defines the function signature for event handlers
type EventHandler func(event Event)

// EventManager manages event registration and triggering
type EventManager struct {
	handlers map[string][]EventHandler
	mu       sync.Mutex
}

// NewEventManager creates a new instance of EventManager
func NewEventManager() *EventManager {
	return &EventManager{
		handlers: make(map[string][]EventHandler),
	}
}

// RegisterEventHandler registers an event handler for a specific event type
func (em *EventManager) RegisterEventHandler(eventType string, handler EventHandler) {
	em.mu.Lock()
	defer em.mu.Unlock()

	if _, exists := em.handlers[eventType]; !exists {
		em.handlers[eventType] = []EventHandler{}
	}
	em.handlers[eventType] = append(em.handlers[eventType], handler)
}

// TriggerEvent triggers all handlers registered for the given event type
func (em *EventManager) TriggerEvent(event Event) {
	em.mu.Lock()
	defer em.mu.Unlock()

	if handlers, exists := em.handlers[event.Type]; exists {
		for _, handler := range handlers {
			go handler(event) // Execute handlers concurrently
		}
	} else {
		fmt.Printf("No handlers registered for event type: %s\n", event.Type)
	}
}

// Example usage
// func main() {
// 	em := NewEventManager()
// 
// 	// Register an event handler
// 	em.RegisterEventHandler("file.created", func(event Event) {
// 		fmt.Printf("File created: %+v\n", event.Data)
// 	})
// 
// 	// Trigger an event
// 	em.TriggerEvent(Event{
// 		Type: "file.created",
// 		Data: map[string]string{"filename": "example.txt"},
// 	})
// }