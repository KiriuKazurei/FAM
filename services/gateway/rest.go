package gateway

import (
	"encoding/json"
	"net/http"
	"log"

	"github.com/gorilla/mux"
)


// Middleware for logging and error handling
func loggingMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		log.Printf("Request: %s %s", r.Method, r.URL.Path)
		next.ServeHTTP(w, r)
	})
}

// Standardized response structure
type Response struct {
	Status  string      `json:"status"`
	Message string      `json:"message,omitempty"`
	Data    interface{} `json:"data,omitempty"`
}

// Helper function to send JSON response
func respondWithJSON(w http.ResponseWriter, statusCode int, response Response) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(statusCode)
	json.NewEncoder(w).Encode(response)
}

// Example handler for file management
func handleFileList(w http.ResponseWriter, r *http.Request) {
	// Simulate fetching file list
	files := []string{"file1.jpg", "file2.png", "file3.raw"}
	respondWithJSON(w, http.StatusOK, Response{
		Status: "success",
		Data:   files,
	})
}

// Example handler for tag management
func handleTagUpdate(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	fileID := vars["id"]

	// Simulate updating tags for a file
	respondWithJSON(w, http.StatusOK, Response{
		Status:  "success",
		Message: "Tags updated for file ID: " + fileID,
	})
}

// Initialize REST adapter
func InitRESTAdapter() *mux.Router {
	r := mux.NewRouter()

	// Middleware
	r.Use(loggingMiddleware)

	// Routes
	r.HandleFunc("/files", handleFileList).Methods("GET")
	r.HandleFunc("/files/{id}/tags", handleTagUpdate).Methods("PUT")

	// Health check endpoint
	r.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		respondWithJSON(w, http.StatusOK, Response{
			Status: "healthy",
		})
	}).Methods("GET")

	return r
}
