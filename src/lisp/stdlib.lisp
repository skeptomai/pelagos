;;; Remora Lisp standard macros — embedded at compile time, evaluated at startup.
;;;
;;; These macros are available in every .reml file without any import.

;; ── c[ad]+r family (3- and 4-level compositions) ─────────────────────────
;;
;; 2-level forms (caar cadr cdar cddr) and caddr are builtins.
;; The remainder through 4 levels follow here per R7RS convention.

(define (caaar  x) (car (car (car x))))
(define (caadr  x) (car (car (cdr x))))
(define (cadar  x) (car (cdr (car x))))
(define (cdddr  x) (cdr (cdr (cdr x))))
(define (cdaar  x) (cdr (car (car x))))
(define (cdadr  x) (cdr (car (cdr x))))
(define (cddar  x) (cdr (cdr (car x))))

(define (caaaar x) (car (car (car (car x)))))
(define (caaadr x) (car (car (car (cdr x)))))
(define (caadar x) (car (car (cdr (car x)))))
(define (caaddr x) (car (car (cdr (cdr x)))))
(define (cadaar x) (car (cdr (car (car x)))))
(define (cadadr x) (car (cdr (car (cdr x)))))
(define (caddar x) (car (cdr (cdr (car x)))))
(define (cadddr x) (car (cdr (cdr (cdr x)))))
(define (cdaaar x) (cdr (car (car (car x)))))
(define (cdaadr x) (cdr (car (car (cdr x)))))
(define (cdadar x) (cdr (car (cdr (car x)))))
(define (cdaddr x) (cdr (car (cdr (cdr x)))))
(define (cddaar x) (cdr (cdr (car (car x)))))
(define (cddadr x) (cdr (cdr (car (cdr x)))))
(define (cdddar x) (cdr (cdr (cdr (car x)))))
(define (cddddr x) (cdr (cdr (cdr (cdr x)))))

;; (define-service var-name "svc-name"
;;   :image    "img"
;;   :network  "net"
;;   :port     (host-var . container)            ; host maps to container
;;   :port     (8080 . 80) (2003 . 2003)         ; multiple ports under one :port
;;   :env      ("KEY1" . "val1") ("KEY2" . var)  ; host env maps to container env
;;   :bind     ("./host/path" . "/container/path") ; read-only (safe default)
;;   :bind-rw  ("./host/path" . "/container/path") ; read-write (explicit opt-in)
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

;; (assoc key alist) — find first pair in alist whose car equals key (string=?).
;; Returns the pair (key . value) or #f if not found.
(define (assoc key lst)
  (cond ((null? lst) #f)
        ((string=? key (car (car lst))) (car lst))
        (else (assoc key (cdr lst)))))

;; (result-ref results name) — extract a handle from a run-all alist by service name.
(define (result-ref results name)
  (let ((entry (assoc name results)))
    (if entry
      (cdr entry)
      (errorf "result-ref: no result for '~a'" name))))

;; (zero? x) — true if x is equal to 0.
(define (zero? x) (= x 0))

;; (logf fmt arg...) — format a string and log it.
;; Equivalent to (log (format fmt arg...)).
(defmacro logf (fmt . args)
  `(log (format ,fmt ,@args)))

;; (errorf fmt arg...) — format a string and raise it as an error.
;; Equivalent to (error (format fmt arg...)).
(defmacro errorf (fmt . args)
  `(error (format ,fmt ,@args)))

;; (unless condition body...)
;; Runs body when condition is false.  Equivalent to (when (not condition) body...).
(defmacro unless (condition . body)
  `(when (not ,condition) ,@body))

;; ── Result type ───────────────────────────────────────────────────────────
;;
;; A lightweight Result<T,E> type represented as a tagged list.
;;
;;   (ok value)    — success; wraps a return value
;;   (err reason)  — failure; wraps an error string
;;
;; Predicates: (ok? r)  (err? r)
;; Accessors:  (ok-value r)  (err-reason r)

(define (ok  value)  (list 'ok  value))
(define (err reason) (list 'err reason))

(define (ok?  r) (and (pair? r) (eq? (car r) 'ok)))
(define (err? r) (and (pair? r) (eq? (car r) 'err)))

(define (ok-value   r) (cadr r))
(define (err-reason r) (cadr r))

;; (with-cleanup cleanup body...)
;; Runs body; calls (cleanup result) on any exit where result is either
;; (ok value) on normal exit or (err reason) on error exit.
;; The error is re-raised after cleanup so it still propagates.
(defmacro with-cleanup (cleanup . body)
  `(guard (exn (#t (,cleanup (err exn)) (error exn)))
     (let ((result (begin ,@body)))
       (,cleanup (ok result))
       result)))

;; (define-nodes (var1 svc1) (var2 svc2) ...)
;;
;; Declare multiple lazy start nodes in one form.  Each (var svc) pair
;; expands to (define var (start svc)).  Nothing executes until `run` is called.
;;
;;   (define-nodes
;;     (db    svc-db)
;;     (cache svc-cache))
;;
;; Expands to: (define db (start svc-db)) (define cache (start svc-cache))
(defmacro define-nodes bindings
  `(begin
     ,@(map (lambda (b) `(define ,(car b) (start ,(cadr b))))
            bindings)))

;; (define-results results-var (var1 "key1") (var2 "key2") ...)
;;
;; Destructure an alist returned by `run` into named bindings.
;;
;;   (define-results results
;;     (db-handle  "db")
;;     (app-handle "app"))
;;
;; Expands to individual (define var (result-ref results-var key)) forms.
(defmacro define-results (results-var . bindings)
  `(begin
     ,@(map (lambda (b) `(define ,(car b) (result-ref ,results-var ,(cadr b))))
            bindings)))

;; (define-run [keywords...] (binding-name future-var) ...)
;;
;; Execute a dependency graph and bind the results in one form.  Combines
;; `run` + `define-results` by deriving each result key from the future
;; variable name via `symbol->string`.
;;
;; Keywords (:parallel, :max-parallel N) are any non-list arguments before
;; the first (binding-name future-var) pair.
;;
;;   (define-run :parallel
;;     (db-handle    db)
;;     (cache-handle cache)
;;     (app-handle   app))
;;
;; Expands to:
;;   (begin
;;     (define _run_result_ (run (list db cache app) :parallel))
;;     (define db-handle    (result-ref _run_result_ "db"))
;;     (define cache-handle (result-ref _run_result_ "cache"))
;;     (define app-handle   (result-ref _run_result_ "app")))
;;
;; Convention: the future variable name must match the service name
;; (e.g. future `db` from `(define-service svc-db "db" ...)` has
;; internal name `"db"`).  This is always true when using `define-nodes`.
;; For unusual naming use `define-results` directly.
(defmacro define-run opts-and-bindings

  ;; Split into two lists: keyword atoms come first (symbols, numbers),
  ;; then (binding future-var) pairs.  Stops at the first pair encountered.
  (define (split-args lst)
    (let loop ((rest lst) (kws '()))
      (cond
        ((null? rest)        (cons (reverse kws) '()))
        ((pair? (car rest))  (cons (reverse kws) rest))
        (else                (loop (cdr rest) (cons (car rest) kws))))))

  (let* ((parts    (split-args opts-and-bindings))
         (kws      (car parts))
         (bindings (cdr parts))
         (vars     (map cadr bindings))
         (names    (map car  bindings))
         (keys     (map (lambda (b) (symbol->string (cadr b))) bindings)))
    `(begin
       (define _run_result_ (run (list ,@vars) ,@kws))
       ,@(map (lambda (name key)
                `(define ,name (result-ref _run_result_ ,key)))
              names keys))))

;; (define-then name upstream (param) body...)
;;
;; Combined define + then: define `name` as the node whose value is computed
;; by applying body to the resolved value of `upstream`, with the handle bound
;; to `param`.
;;
;;   (define-then db-url db (h)
;;     (format "postgres://app:secret@~a/appdb" (container-ip h)))
;;
;; Expands to: (define db-url (then db (lambda (h) body...) :name "db-url"))
;;
;; The :name argument makes the future's internal name match the Lisp binding,
;; so error messages reference "db-url" rather than the generated "db-then".
;;
;; For multi-upstream joins use `then-all` directly.
(defmacro define-then (name upstream params . body)
  `(define ,name (then ,upstream (lambda ,params ,@body) :name ,(symbol->string name))))

