package gateway

import (
	"context"
	"log"
	"net"

	pb "proto"

	grpc "google.golang.org/grpc"
)

// gRPC service implementation
type fileServiceServer struct {
	pb.UnimplementedFileServiceServer
}

// ListFiles returns a list of files
func (s *fileServiceServer) ListFiles(ctx context.Context, req *pb.ListFilesRequest) (*pb.ListFilesResponse, error) {
	log.Println("gRPC: ListFiles called")
	// Simulate fetching file list
	files := []string{"file1.jpg", "file2.png", "file3.raw"}
	return &pb.ListFilesResponse{Files: files}, nil
}

// UpdateTags updates tags for a specific file
func (s *fileServiceServer) UpdateTags(ctx context.Context, req *pb.UpdateTagsRequest) (*pb.UpdateTagsResponse, error) {
	log.Printf("gRPC: UpdateTags called for file ID: %s\n", req.FileId)
	// Simulate updating tags
	return &pb.UpdateTagsResponse{Message: "Tags updated for file ID: " + req.FileId}, nil
}

// Start gRPC server
func StartGRPCServer(port string) {
	lis, err := net.Listen("tcp", ":"+port)
	if err != nil {
		log.Fatalf("Failed to listen: %v", err)
	}
	s := grpc.NewServer()
	pb.RegisterFileServiceServer(s, &fileServiceServer{})
	log.Printf("gRPC server listening on port %s", port)
	if err := s.Serve(lis); err != nil {
		log.Fatalf("Failed to serve: %v", err)
	}
}
