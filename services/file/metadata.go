package file

import (
	"fmt"

	"path/filepath"

	image "FAM/core/imageutil"
)

type ImageMetadata struct {
	Path     string
	Format   string
	Width    int
	Height   int
	Camera   string                 // 相机型号（make+model）
	RawData  map[string]interface{} `json:",omitempty"` // 原始RAW数据
	Standard map[string]interface{} `json:",omitempty"` // 标准元数据
	BitDepth int                    // 新增位深度字段
}

func ExtractExif(filePath string) (*ImageMetadata, error) {
	metaMap, err := image.ProcessImage(filePath)
	if err != nil {
		return nil, fmt.Errorf("处理图像失败: %w", err)
	}

	imd := &ImageMetadata{
		Path:   filePath,
		Format: filepath.Ext(filePath),
	}

	if stdMeta, ok := metaMap["standard_metadata"].(map[string]interface{}); ok {
		imd.Standard = stdMeta
		imd.Width = stdMeta["width"].(int)
		imd.Height = stdMeta["height"].(int)
		imd.BitDepth = stdMeta["bit_depth"].(int) // 新增位深度赋值
	} else if rawMeta, ok := metaMap["raw_metadata"].(map[string]interface{}); ok {
		imd.RawData = rawMeta
		imd.Camera = fmt.Sprintf("%v %v", rawMeta["camera_make"], rawMeta["camera_model"])
	}

	return imd, nil
}
