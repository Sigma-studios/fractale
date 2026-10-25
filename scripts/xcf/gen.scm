; Builds the XCF fixtures and GIMP's own flattened PNG of each.
; Run by tools/gen.sh inside the VM (GIMP 2.10, Script-Fu, batch mode).
; `outdir` is defined by the caller before this file is loaded.

(define (out name ext) (string-append outdir name ext))

; Flatten a duplicate the way GIMP composites the canvas, then normalise it
; to 8-bit sRGB RGBA so the Rust test only has to read one PNG flavour.
; Indexed images go to RGB first: merging in indexed mode would snap the
; blended pixels to the palette, which the on-screen projection doesn't do.
(define (export-png img name)
  (let* ((dup (car (gimp-image-duplicate img))))
    (if (= (car (gimp-image-base-type dup)) INDEXED) (gimp-image-convert-rgb dup))
    (gimp-image-merge-visible-layers dup CLIP-TO-IMAGE)
    (if (not (= (car (gimp-image-base-type dup)) RGB)) (gimp-image-convert-rgb dup))
    (if (not (= (car (gimp-image-get-precision dup)) 150)) (gimp-image-convert-precision dup 150))
    (let ((layer (car (gimp-image-get-active-layer dup))))
      (gimp-layer-resize-to-image-size layer)
      (if (= (car (gimp-drawable-has-alpha layer)) 0) (gimp-layer-add-alpha layer))
      (file-png-save2 RUN-NONINTERACTIVE dup layer (out name ".png") (out name ".png")
                      0 9 0 0 0 0 0 0 1))
    (gimp-image-delete dup)))

(define (save img name)
  (let ((d (car (gimp-image-get-active-drawable img))))
    (gimp-xcf-save 0 img d (out name ".xcf") (out name ".xcf"))
    (export-png img name)))

(define (add-layer img type w h name opacity x y parent)
  (let ((l (car (gimp-layer-new img w h type name opacity LAYER-MODE-NORMAL))))
    (gimp-image-insert-layer img l parent 0)  ; top of `parent`, not beside the active layer
    (gimp-layer-set-offsets l x y)
    l))

(define (gradient d x1 y1 x2 y2 fg bg)
  (gimp-context-set-foreground fg)
  (gimp-context-set-background bg)
  (gimp-context-set-gradient "FG to BG (RGB)")
  (gimp-drawable-edit-gradient-fill d GRADIENT-LINEAR 0 FALSE 1 0 FALSE x1 y1 x2 y2))

; Selections take image coordinates; these helpers take layer coordinates.
(define (at d x y) (let ((o (gimp-drawable-offsets d))) (list (+ x (car o)) (+ y (cadr o)))))

(define (ellipse img d x y w h color)
  (let ((p (at d x y))) (gimp-image-select-ellipse img CHANNEL-OP-REPLACE (car p) (cadr p) w h))
  (gimp-context-set-foreground color)
  (gimp-drawable-edit-fill d FILL-FOREGROUND)
  (gimp-selection-none img))

(define (rect img d x y w h color)
  (let ((p (at d x y))) (gimp-image-select-rectangle img CHANNEL-OP-REPLACE (car p) (cadr p) w h))
  (gimp-context-set-foreground color)
  (gimp-drawable-edit-fill d FILL-FOREGROUND)
  (gimp-selection-none img))

; A background plus a half-opaque offset layer, on a w*h RGB image.
(define (top-layer img w h)
  (let ((top (add-layer img RGBA-IMAGE (/ w 2) (/ h 2) "top" 50 (/ w 3) (/ h 4) 0)))
    (ellipse img top 0 0 (/ w 2) (/ h 2) '(230 20 20))
    (rect img top 2 2 6 6 '(255 255 255))
    top))

(define (two-layers img w h)
  (let ((bg (add-layer img RGB-IMAGE w h "bg" 100 0 0 0)))
    (gradient bg 0 0 0 h '(20 40 200) '(250 220 30))
    (ellipse img bg (/ w 2) 2 (/ w 3) (/ h 3) '(10 10 10))
    (top-layer img w h)))

(gimp-context-set-antialias TRUE)

; 8-bit RGB, RLE, 3x2 tiles with clipped edge tiles, offset half-opacity
; layer, a hidden layer and a legacy-mode layer hanging off the left/bottom edges.
(let ((img (car (gimp-image-new 130 90 RGB))))
  (two-layers img 130 90)
  (let ((hidden (add-layer img RGBA-IMAGE 40 40 "hidden" 100 5 5 0))
        (edge (add-layer img RGBA-IMAGE 40 30 "edge" 100 -10 70 0)))
    (gimp-drawable-fill hidden FILL-WHITE)
    (gimp-item-set-visible hidden FALSE)
    (ellipse img edge 0 0 40 30 '(0 200 200))
    ; GIMP 2.8-style mode: composited on gamma-encoded values, not linear light.
    (gimp-layer-set-mode edge LAYER-MODE-NORMAL-LEGACY)
    (gimp-layer-set-opacity edge 60))
  (save img "rgb_layers"))

; Grayscale, two layers.
(let ((img (car (gimp-image-new 40 30 RGB))))
  (two-layers img 40 30)
  (gimp-image-convert-grayscale img)
  (save img "gray"))

; Indexed with a generated 16-colour palette; the top layer gets binary alpha.
(let ((img (car (gimp-image-new 40 30 RGB))))
  (two-layers img 40 30)
  (gimp-image-convert-indexed img CONVERT-DITHER-NONE CONVERT-PALETTE-GENERATE 16 FALSE FALSE "")
  (save img "indexed"))

; High bit depth: convert first so the gradients are drawn at full precision.
(define (deep name precision)
  (let* ((img (car (gimp-image-new 40 30 RGB))))
    (gimp-image-convert-precision img precision)
    (two-layers img 40 30)
    (save img name)))
(deep "u16_gamma" 250)
(deep "float_linear" 600)
(deep "half_gamma" 550)
(deep "u32_linear" 300)

; Layer group (opacity 70) with two overlapping children, and a layer mask.
(let* ((img (car (gimp-image-new 50 40 RGB)))
       (bg (add-layer img RGB-IMAGE 50 40 "bg" 100 0 0 0))
       (grp (car (gimp-layer-group-new img))))
  (gimp-drawable-fill bg FILL-WHITE)
  (gimp-image-insert-layer img grp 0 0)
  (gimp-layer-set-opacity grp 70)
  (let ((a (add-layer img RGBA-IMAGE 30 30 "a" 100 2 2 grp))
        (b (add-layer img RGBA-IMAGE 30 30 "b" 100 15 8 grp)))
    (ellipse img a 0 0 30 30 '(220 0 0))
    (rect img b 0 0 30 30 '(0 0 220)))
  (let* ((m (add-layer img RGBA-IMAGE 50 12 "masked" 100 0 26 0))
         (mask (car (gimp-layer-create-mask m ADD-MASK-WHITE))))
    (rect img m 0 0 50 12 '(0 160 0))
    (gimp-layer-add-mask m mask)
    (gradient mask 0 0 50 0 '(0 0 0) '(255 255 255)))
  (save img "group_mask"))

; zlib: GIMP keeps a loaded image's tile compression, so start from the
; hand-made zlib seed and grow it.
(define (from-seed name precision)
  (let* ((img (car (gimp-file-load RUN-NONINTERACTIVE (out "seed_zlib" ".xcf") "seed")))
         (seed (car (gimp-image-get-active-layer img))))
    (if (not (= precision 150)) (gimp-image-convert-precision img precision))
    (gimp-image-resize img 100 70 0 0)
    (gimp-layer-resize-to-image-size seed)
    (gradient seed 0 0 100 0 '(250 250 250) '(120 0 120))
    (top-layer img 100 70)
    (save img name)))
(from-seed "zlib" 150)
(from-seed "zlib_half_linear" 500)

; The hand-made version-0 uncompressed file: only GIMP's rendering is needed.
(let ((img (car (gimp-file-load RUN-NONINTERACTIVE (out "v0_raw" ".xcf") "v0"))))
  (export-png img "v0_raw"))
