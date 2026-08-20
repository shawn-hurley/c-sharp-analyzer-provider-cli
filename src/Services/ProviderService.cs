using CSharpProvider.Analysis;
using Google.Protobuf.WellKnownTypes;
using Grpc.Core;
using Provider;

namespace CSharpProvider.Services;

// Provider.ProviderService.ProviderServiceBase is the base class generated from
// provider.proto.
public static class AnalysisMode
{
    public const string SourceOnly = "source-only";
    public const string Full = "full";
}

public class ProviderService : Provider.ProviderService.ProviderServiceBase
{
    private readonly ProviderConfig _config;
    private readonly ILogger<ProviderService> _logger;
    private readonly PackageResolver _packageResolver;
    private readonly ProjectStateHolder _stateHolder;

    public ProviderService(
        ProviderConfig config,
        ILogger<ProviderService> logger,
        PackageResolver packageResolver,
        ProjectStateHolder stateHolder)
    {
        _config = config;
        _logger = logger;
        _packageResolver = packageResolver;
        _stateHolder = stateHolder;
    }

    public override Task<CapabilitiesResponse> Capabilities(
        Empty request, ServerCallContext context)
    {
        var response = new CapabilitiesResponse();
        response.Capabilities.Add(new Capability { Name = "referenced" });
        return Task.FromResult(response);
    }

    public override async Task<InitResponse> Init(
        Config request, ServerCallContext context)
    {
        _logger.LogInformation("Init called with location={Location}, analysisMode={Mode}",
            request.Location, request.AnalysisMode);

        if (request.AnalysisMode == AnalysisMode.Full)
        {
            _logger.LogWarning(
                "Analysis mode '{Mode}' is not supported. Running source-only analysis",
                request.AnalysisMode);
        }

        try
        {
            var loader = new ProjectLoader(_logger, _packageResolver);
            var compilation = await loader.LoadAsync(request.Location, context.CancellationToken);

            _stateHolder.Set(new ProjectState(compilation, request.Location));

            _logger.LogInformation("Project loaded successfully with {TreeCount} syntax trees",
                compilation.SyntaxTrees.Count());

            return new InitResponse
            {
                Successful = true,
                Id = 1,
                BuiltinConfig = new Config
                {
                    Location = request.Location,
                    AnalysisMode = AnalysisMode.SourceOnly
                }
            };
        }
        catch (Exception ex)
        {
            _logger.LogError(ex, "Failed to initialize project");
            return new InitResponse
            {
                Successful = false,
                Error = ex.Message,
            };
        }
    }

    public override Task<EvaluateResponse> Evaluate(EvaluateRequest request, ServerCallContext context)
    {
        _logger.LogDebug("Evaluate called: cap={Cap}, id={Id}", request.Cap, request.Id);

        var state = _stateHolder.Get();
        if (state == null)
        {
            return Task.FromResult(new EvaluateResponse
            {
                Successful = false,
                Error = "Provider not initialized. Call Init first.",
            });
        }

        if (request.Cap != "referenced")
        {
            return Task.FromResult(new EvaluateResponse
            {
                Successful = false,
                Error = $"Unsupported capability: {request.Cap}",
            });
        }

        try
        {
            var query = SymbolQuery.ParseCondition(request.ConditionInfo);
            var results = SymbolQuery.Execute(state.Compilation, state.ProjectPath, query);
            var deduplicated = SymbolQuery.Deduplicate(results);
            var incidents = SymbolQuery.ToIncidentContexts(deduplicated);

            var evalResponse = new ProviderEvaluateResponse { Matched = incidents.Count > 0 };
            evalResponse.IncidentContexts.Add(incidents);

            return Task.FromResult(new EvaluateResponse
            {
                Successful = true,
                Response = evalResponse,
            });
        }
        catch (Exception ex)
        {
            _logger.LogError(ex, "Evaluate failed");
            return Task.FromResult(new EvaluateResponse
            {
                Successful = false,
                Error = ex.Message,
            });
        }
    }

    public override Task<Empty> Stop(ServiceRequest request, ServerCallContext context)
    {
        _logger.LogInformation("Stop called");
        _stateHolder.Set(null);
        return Task.FromResult(new Empty());
    }

    public override Task<NotifyFileChangesResponse> NotifyFileChanges(
        NotifyFileChangesRequest request, ServerCallContext context)
    {
        _logger.LogInformation("NotifyFileChanges called with {Count} changes", request.Changes.Count);

        var state = _stateHolder.Get();
        if (state == null)
        {
            return Task.FromResult(new NotifyFileChangesResponse
            {
                Error = "Provider not initialized",
            });
        }

        _stateHolder.Update(s =>
        {
            var compilation = s.Compilation;

            foreach (var change in request.Changes)
            {
                var uri = change.Uri;
                var filePath = uri.StartsWith("file://") ? new Uri(uri).LocalPath : uri;

                var oldTree = compilation.SyntaxTrees
                    .FirstOrDefault(t => t.FilePath == filePath);

                if (oldTree != null)
                {
                    var newTree = Microsoft.CodeAnalysis.CSharp.CSharpSyntaxTree.ParseText(
                        change.Content, path: filePath);
                    compilation = compilation.ReplaceSyntaxTree(oldTree, newTree);
                }
                else
                {
                    var newTree = Microsoft.CodeAnalysis.CSharp.CSharpSyntaxTree.ParseText(
                        change.Content, path: filePath);
                    compilation = compilation.AddSyntaxTrees(newTree);
                }
            }

            return new ProjectState(compilation, s.ProjectPath);
        });

        return Task.FromResult(new NotifyFileChangesResponse());
    }

    public override Task<DependencyResponse> GetDependencies(ServiceRequest request, ServerCallContext context)
    {
        return Task.FromResult(new DependencyResponse { Successful = true });
    }

    public override Task<DependencyDAGResponse> GetDependenciesDAG(ServiceRequest request, ServerCallContext context)
    {
        return Task.FromResult(new DependencyDAGResponse { Successful = true });
    }

    public override Task<PrepareResponse> Prepare(PrepareRequest request, ServerCallContext context)
    {
        return Task.FromResult(new PrepareResponse());
    }

    public override async Task StreamPrepareProgress(
        PrepareProgressRequest request,
        IServerStreamWriter<ProgressEvent> responseStream,
        ServerCallContext context)
    {
        await Task.CompletedTask;
    }
}
