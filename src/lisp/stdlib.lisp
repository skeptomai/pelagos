;;; Remora Lisp standard macros — embedded at compile time, evaluated at startup.
;;;
;;; These macros are available in every .reml file without any import.

;; (define-service var-name "svc-name"
;;   :image    "img"
;;   :network  "net"
;;   :port     (host-var . container)            ; host maps to container
;;   :port     (8080 . 80) (2003 . 2003)         ; multiple ports under one :port
;;   :env      ("KEY1" . "val1") ("KEY2" . var)  ; host env maps to container env
;;   :bind-ro  ("./host/path" . "/container/path") ; host path maps to container path
;;   :bind     ("./host/path" . "/container/path") ; same, read-write
;;   :memory   mem-var)
;;
;; The options are a flat keyword-value list.  Each :keyword introduces a new
;; option; its values run until the next :keyword or end of list.
;;
;; If a keyword's values are all sublists (e.g. :env pairs), each sublist is
;; expanded into a separate service option:
;;   :env ("A" "1") ("B" "2")  →  (list 'env "A" "1") (list 'env "B" "2")
;;
;; If a keyword's values are atoms or mixed, they all go into one option:
;;   :port host 80             →  (list 'port host 80)
;;   :depends-on "redis" 6379  →  (list 'depends-on "redis" 6379)
;;
;; The practical reason proper lists are used for multi-value entries (rather
;; than dotted pairs) is that unquote-splicing `,@sub` only works on proper
;; lists.  Dotted pairs are semantically purer for key-value data but require
;; explicit (car)/(cdr) in the macro template instead of clean splice syntax.

(defmacro define-service (var-name svc-name . opts)

  ;; True if v is a :keyword symbol (starts with ':').
  (define (kw? v)
    (and (symbol? v)
         (string=? (substring (symbol->string v) 0 1) ":")))

  ;; Split a flat keyword-value list into groups.
  ;; (:a 1 2 :b 3 :c (x) (y)) → ((:a 1 2) (:b 3) (:c (x) (y)))
  (define (split-opts lst)
    (if (null? lst) '()
        (let loop ((rest   (cdr lst))
                   (kw     (car lst))
                   (vals   '())
                   (result '()))
          (cond
            ((null? rest)
             (reverse (cons (cons kw (reverse vals)) result)))
            ((kw? (car rest))
             (loop (cdr rest)
                   (car rest)
                   '()
                   (cons (cons kw (reverse vals)) result)))
            (else
             (loop (cdr rest) kw (cons (car rest) vals) result))))))

  ;; Convert one group (kw val...) into a list of (list 'key args...) forms.
  ;; Returns a list so multi-value groups (like :env) can yield multiple forms.
  ;;
  ;; Values may be:
  ;;   atoms/exprs  — all grouped into one option: :memory mem-var → (list 'memory mem-var)
  ;;   proper lists — one option per sublist, spliced: :env ("K" "v") → (list 'env "K" "v")
  ;;   dotted pairs — one option per pair, car/cdr:  :env ("K" . "v") → (list 'env "K" "v")
  (define (expand-opt group)
    (let* ((kw   (car group))
           (sym  (string->symbol (substring (symbol->string kw) 1)))
           (args (cdr group)))
      (if (and (not (null? args)) (pair? (car args)))
          ;; All values are pairs (proper or dotted) — one service option per sub-value.
          (map (lambda (sub)
                 (if (list? sub)
                     `(list ',sym ,@sub)              ; proper list: splice all items
                     `(list ',sym ,(car sub) ,(cdr sub)))) ; dotted pair: key . val
               args)
          ;; Atom values → a single service option with all args.
          (list `(list ',sym ,@args)))))

  `(define ,var-name
     (service ,svc-name ,@(apply append (map expand-opt (split-opts opts))))))
